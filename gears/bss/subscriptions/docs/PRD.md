---
refs:
  - bss/manifest/vz-arch-manifest-bss-only.md
  - bss/prd/PRD-billing-ledger-balances-202604041200
  - bss/prd/PRD-billing-module-202601120119
  - bss/prd/PRD-contracts-agreements-202601120119
  - bss/prd/PRD-plan-price-modeling-202605281200
  - bss/prd/PRD-product-catalog-marketplace-202601120119
  - bss/prd/PRD-product-sku-management-202606101924
  - bss/prd/PRD-rating-engine-202604031200
  - bss/prd/PRD-subscriptions-entitlements-202601120119
  - bss/prd/PRD-tariffs-pricing-logic-202604011200
---

Created:  2026-08-24 by Virtuozzo International GmbH
Updated:  2026-10-02 by Virtuozzo International GmbH

# PRD — Subscriptions — End-to-End Lifecycle (Multi-Tenant Revenue Object)

<!-- CONFLUENCE_TITLE: [BSS]: Subscriptions — End-to-End Lifecycle (Multi-Tenant Revenue Object) -->
<!-- Vendored from vhp-architecture: PR #154 (branch VHP-806, commit 4faef39652d0, PRD-subscriptions-lifecycle-202604021200) converted to this repo's PRD format and MERGED 2026-07-15 with the predecessor PRD-subscriptions-entitlements-202601120119 (upstream main) into the single normative PRD of the subscriptions gear (one gear — one PRD, mirroring the rating consolidation). Originally vendored from upstream vhp-architecture; gears-rust is now the canonical home and upstream is not maintained. Owners: BSS Subscriptions team -->

<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Background / Problem Statement](#12-background--problem-statement)
  - [1.3 Goals (Business Outcomes)](#13-goals-business-outcomes)
  - [1.4 Glossary](#14-glossary)
- [2. Architecture Alignment](#2-architecture-alignment)
  - [2.1 Terminology and Naming](#21-terminology-and-naming)
  - [2.2 Predecessor PRDs and Scope Migration](#22-predecessor-prds-and-scope-migration)
- [3. Actors](#3-actors)
  - [3.1 Human Actors](#31-human-actors)
  - [3.2 System Actors](#32-system-actors)
- [4. Operational Concept & Environment](#4-operational-concept--environment)
  - [4.1 Module-Specific Environment Constraints](#41-module-specific-environment-constraints)
- [5. Scope](#5-scope)
  - [5.1 In Scope](#51-in-scope)
  - [5.2 Out of Scope](#52-out-of-scope)
- [6. Functional Requirements](#6-functional-requirements)
  - [6.1 Lifecycle State Machine](#61-lifecycle-state-machine)
  - [6.2 Versioning and Effective-Dated Composition](#62-versioning-and-effective-dated-composition)
  - [6.3 Plan Changes (Upgrade / Downgrade)](#63-plan-changes-upgrade--downgrade)
  - [6.4 Suspension and Reactivation](#64-suspension-and-reactivation)
  - [6.5 Renewal and Grace](#65-renewal-and-grace)
  - [6.6 Multi-Tenant Ownership](#66-multi-tenant-ownership)
  - [6.7 Event Model](#67-event-model)
  - [6.8 Billing Alignment](#68-billing-alignment)
  - [6.9 Entitlement Posture and Issuance](#69-entitlement-posture-and-issuance)
  - [6.10 Trial Runtime and Conversion](#610-trial-runtime-and-conversion)
  - [6.11 Data Retention and Offboarding](#611-data-retention-and-offboarding)
- [7. Non-Functional Requirements](#7-non-functional-requirements)
  - [7.1 NFR Inclusions](#71-nfr-inclusions)
  - [7.2 NFR Exclusions](#72-nfr-exclusions)
- [8. Five Quality Vectors Analysis](#8-five-quality-vectors-analysis)
- [9. Public Library Interfaces](#9-public-library-interfaces)
  - [9.1 Public API Surface](#91-public-api-surface)
  - [9.2 External Integration Contracts](#92-external-integration-contracts)
- [10. Use Cases](#10-use-cases)
- [11. User Interaction and Design](#11-user-interaction-and-design)
- [12. Acceptance Criteria](#12-acceptance-criteria)
  - [Lifecycle and guards](#lifecycle-and-guards)
  - [Composition and billing](#composition-and-billing)
  - [Renewal and activation eligibility](#renewal-and-activation-eligibility)
  - [Versioning, multi-tenant, and events](#versioning-multi-tenant-and-events)
  - [Entitlements and trials](#entitlements-and-trials)
  - [Scheduled intents, quantity, pause, activation instants](#scheduled-intents-quantity-pause-activation-instants)
  - [Ordering pin, recurring split, void, pause interplay](#ordering-pin-recurring-split-void-pause-interplay)
  - [Orders, contracts, and usage attribution](#orders-contracts-and-usage-attribution)
  - [Data, terms, co-termination, and import](#data-terms-co-termination-and-import)
- [13. Dependencies](#13-dependencies)
- [14. Assumptions](#14-assumptions)
- [15. Open Questions](#15-open-questions)
- [16. Risks](#16-risks)
- [17. Reference Materials](#17-reference-materials)
  - [17.1 Reconciliation Framework (operational appendix)](#171-reconciliation-framework-operational-appendix)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

**Subscriptions** is the BSS gear that owns the **subscription** as the **primary commercial aggregate** for recurring revenue: a versioned, auditable **lifecycle state machine** with **effective-dated composition** (`PlanLink`, `AddOn`) that **aligns** Rating (usage + rated charges) and Billing (recurring line items, tax, GL, ASC inputs) under **multi-tenant** ownership (`resourceTenantId`, `payerTenantId`, `sellerTenantId`).

This gear owns the lifecycle **engine** — state machine, versioning and snapshots, renewal and failed-renewal/grace, events and ordering, multi-tenant delegation — **not** the catalog primitives it composes (`Plan`/`Price`/`PriceWindow` are the Pricing gear's), **not** the proration/evaluation math (the Rating gear's), and **not** posting or invoice immutability enforcement (Billing's). Since the 2026-07-15 consolidation it is also the normative home for the **entitlement posture** — what a subscription confers at any instant and the issuance request that makes License Manager materialize it (§6.9; issuance, checks and counting are the platform licensing gears', re-split 2026-10-02) — and for **trial runtime & conversion** (§6.10), absorbed from the predecessor Subscriptions & Entitlements PRD (§2.2).

Commercially initiated acquisitions and increases reach this gear from the **Orders** gears (Orders Lifecycle, Orders Workflow, Change Orders): a fulfilled order line spawns exactly one subscription through a two-phase create-then-activate pair, and a change order applies one transactional change to an existing subscription (§6.1, §6.3). System-driven transitions — renewal, automatic trial conversion, dunning-driven suspension — stay direct and produce no order. A subscription is governed by a Contract where one is bound and by the published **platform defaults** otherwise; the uncontracted subscription is a first-class state, not a stopgap (§6.5).

### 1.2 Background / Problem Statement

The predecessor `PRD-subscriptions-entitlements-202601120119` describes the **Subscriptions + Entitlements module** end-to-end (PRD-0001 fit, trials, **enforcement at point of use**, SLAs). This PRD is the **manifest-first, production-ready lifecycle specification** for the subscription **commercial aggregate**: manifest §4.3 **state machine**, **versioning** and snapshots, **Billing/Rating** alignment, **renewal** and **failed-renewal/grace**, **events** and ordering, **multi-tenant delegation** — so the lifecycle **engine** has a single normative home.

Without a single normative lifecycle home, transitions, renewal ladders, and proration triggers get re-invented per consumer, posted invoices get silently mutated, and partner-facing billing becomes unexplainable. This PRD fixes the state machine, the composition/versioning discipline, the renewal/grace ladder, the event inventory and ordering, and the Billing alignment invariants so Design implements — not invents — those rules.

The predecessor additionally carried the **entitlement framework** (feature flags, usage/resource quotas, soft/hard limits, the p95 < 100ms check API), **trial management**, renewal notices, and the module SLA table. On 2026-07-15 this repository **consolidated both documents into this single PRD** — the "until re-homed" split was a sequencing artifact of the upstream rewrite, not an architectural boundary, and it cut through the middle of one domain concept (Entitlement). §2.2 records what each predecessor section became and which concerns were superseded to sibling gears; upstream (vhp-architecture) still keeps two documents.

### 1.3 Goals (Business Outcomes)

**Engineering outcomes**

- **Deterministic** transitions with explicit **guards**, **Policy Engine** pre-checks, and **OSS provisioning** confirmation where required by manifest §4.3.
- **Subscription versioning** so historical bill runs reproduce **without** re-interpreting mutable catalog state (snapshot refs per the §4.1 contract).
- **Plan changes** (upgrade/downgrade) with **proration triggers** coordinated between Subscriptions and Billing (not silent invoice mutation).
- **Renewal** semantics aligned with **Contract** `Renewal.autoRenew` and term windows (§4.6), with explicit handling of **failed renewal** (grace, suspension policy).
- **CFO-grade** audit: immutable lineage of state changes, entitlement issuance/revocation, and financial artifact linkage (ASC **inputs** via tags/snapshots; recognition engines out of scope).

**Partner / operator outcomes** (Alternative Cloud Providers and their operators)

- **Predictable billing for partners**: plan changes materialize **proration** as **new billable or adjusting artifacts**, not silent edits to posted invoices — partners can **explain charges** to end customers with a clear paper trail.
- **Transparent failure handling**: **failed renewal** and grace paths emit **auditable** events so partners have **time and signal** to intervene before service is degraded or suspended per policy.
- **Delegation-safe operations**: cross-tenant admin actions require **auditable delegation proofs**, reducing risk of **unauthorized** changes across reseller hierarchies.

**SLA baseline (actionable):** module SLAs from the predecessor (**p95 < 1s** lifecycle synchronous commit class, **daily by 00:00** recurring generation cut, **100%** proration/plan-change monetary accuracy vs policy) are **carried forward** as §7.1 NFR baselines (the predecessor's **p95 < 100ms** entitlement-check figure is not: the check moved to the platform `license-resolver`, §6.9) and **MUST be reconciled** with the program NFR workshop when published (§15).

### 1.4 Glossary

| **Term** | **Definition** |
|----------|------------------|
| **AddOn** | Optional add-on product attached to a subscription with an effective window (`start`/`end` or equivalent); composes with **`PlanLink`** for commercial/rating context (manifest §4.3). |
| **Billing anchor** | UTC instant or calendar rule fixing cycle boundaries (`billingAnchor` on Subscription per manifest data model); drives recurring `BillableItem` period alignment. |
| **Cancellation policy** | Rules governing how a cancel takes effect — immediate, end-of-term, or at a date — and refund/credit eligibility. Modeled as **scheduled lifecycle intents** (§6.1, SUB-D-01); credits materialize only as Billing artifacts. |
| **Change intent** | One transactional request from Orders Workflow applying a change order to an existing subscription: several quantity increases and component additions, confirmed or failed as a whole, guarded by an **expected revision** (§6.3, `fr-composite-change`). |
| **Co-term date** | The date a new subscription's first term ends so that it lines up with a contract term or a payer-level date; later terms keep their normal length aligned to it (§6.5 `fr-co-termination`). |
| **Commercial aggregate** | Domain aggregate rooted at `subscriptionId`; ordering key for CloudEvents per manifest §4.2/§4.3. |
| **Committed usage** | Contractual minimum usage/spend over a term, tracked for true-up. Commitment pools are **Contracts SoR**, evaluated with true-up by the rating gear (T-D-14); this gear keeps subscription-side hooks only (§2.2). |
| **Data retention window** | The period after a cancel during which data-bearing resources are kept in a retention hold, exportable but not in service, before OSS deletes them; platform default 30 days (§6.11 `fr-resource-disposition`). |
| **Entitlement** | Authorization defining what resources, features, or usage limits a customer can access based on the active subscription. Authored as the plan's grant set (pricing gear, incl. the per-phase map), **resolved here** as the subscription's posture (§6.9), **materialized by License Manager** as licenses, packs and limits on this gear's idempotent request, and enforced at point of use by the platform `license-enforcement` and `quota-enforcement` gears (or by the installed product outside the platform). This gear holds no check surface and no counters. |
| **Evaluated fields** | Subscription-stored attributes (e.g. **`graceEndsAt`**, pause flags, ladder variant) computed at **renewal evaluation** time from **Contract** / **`Renewal`** terms for audit, idempotent renewal jobs, and replay. |
| **Evergreen subscription** | Indefinite-term subscription continuing until explicitly cancelled (no fixed end date); terms and notice behavior live on the bound Contract, or on the platform defaults where none is bound (§6.5). |
| **Expected revision** | The subscription `version` a caller asserts as a precondition of a change; a mismatch rejects the change with the subscription unmodified (§6.3). |
| **Feature flag** | Boolean entitlement controlling access to a product feature (enabled/disabled per subscription). |
| **Fixed-term subscription** | Subscription with a defined start/end; may auto-renew or expire at term end (Contract terms; self-service term metadata on the Plan is deferred — pricing §17.8). |
| **Grace period** | Window after a **failed renewal** attempt (pre-check or aligned billing failure) during which the subscription may stay **`active`** while retries/dunning run; duration, billing posture, and exit triggers in §6.5. |
| **Hard limit** | Usage threshold blocking further usage until a quota increase or a new billing cycle; counted and enforced by the platform `quota-enforcement` gear on the limit License Manager provisions from the subscription's posture (§6.9); not tracked here. |
| **idempotencyKey** | Client-supplied identifier on mutating requests; Subscriptions MUST treat duplicate `(subscriptionId, idempotencyKey)` as the same logical operation so exactly **one** durable effect results (manifest §4.3). |
| **Import** | Creation mode that brings a live subscription from a legacy billing system in as it already runs: original start and term, adopted resources, no provisioning and no one-time charge (§6.1 `fr-import`). |
| **Legal hold** | A hold on a subscription or its payer that blocks the purge of its records and the deletion of its retained resources until released (§6.11 `fr-record-retention`). |
| **Order reference** | `orderId`, `orderVersion` and `orderLineId` of the order line that produced or changed a subscription, plus the order's optional external reference (e.g. a purchase-order number). Persisted on the subscription and on the billable facts it emits (§6.1, §6.8). |
| **Order-originated subscription** | A subscription created from a fulfilled `new_sale` order line through the two-phase create-then-activate pair (§6.1, `fr-order-originated-create`). One line spawns exactly one subscription carrying the line's quantity; a bundle plan is one line and one subscription. |
| **Overlap occupancy** | For a `(payerTenantId, overlapScopeKey)`: the number of `active` subscriptions holding the key, the effective `maxConcurrentActive`, and the policy it was resolved from (§6.3). |
| **PlanLink** | Effective-dated link between a subscription and a **catalog plan** (`planId` with `effectiveFrom`/`effectiveTo`); defines which plan applies for rating/billing over each interval (manifest §4.3). The referenced plan/price primitives are authored in the **Pricing gear** ([pricing PRD](../../pricing/docs/PRD.md)). |
| **Plan phase** | Time-bounded segment of a subscription plan (kind **`trial`**, `interim`, or `evergreen` — `interim` was spelled `intro` until pricing D-358, 2026-09-09) with its own price schedule. **Phase structure is Pricing SoR** — the ordered phase set, `convertsToPhaseId` chain, durations and per-phase prices are authored in the catalog and published immutably per plan revision ([pricing PRD](../../pricing/docs/PRD.md) `fr-plan-phases`; 2026-07-31 PR-review fix — this row previously claimed structure for Subscriptions, contradicting the pricing FR it links); **Subscriptions owns the runtime composition and evaluated state**: the effective `PlanLink`s, phase entry/conversion execution from the published durations, and the **active phase at `t`** evaluation resolves — see the **Rating gear** ([rating PRD](../../rating/docs/PRD.md) §17.1 step 1). Trials are modeled as a phase, **not** a `Subscription.status` (§6.1). |
| **PlanTier** | Commercial **tier** implied by the subscribed SKU/plan (e.g. edition/steps); MUST be derivable for any charge instant and remain consistent with **Policy**-gated composition changes (manifest §4.3). The `PlanTier` **taxonomy** is owned by the Catalog registry — the **products** gear (`gears/bss/products/docs/PRD.md`, vendored 2026-07-16). |
| **Platform defaults** | The complete published term set that governs a subscription with no bound Contract: auto-renewal, term, billing anchor instant (the activation instant), notice ladder (30/14/7/1 days), grace (7 days), nonpayment dwell (90 days), acceptance requirement, concurrent-active cardinality, data retention window (30 days), archive dwell (90 days). Tenant-configurable within platform bounds; superseded for a subscription only by an explicit Contract binding (§6.5, `fr-platform-defaults`). |
| **PriceWindow** | Catalog price interval (`planId`/`priceId`, `effectiveFrom`, `effectiveTo`) per BSS manifest §4.1. A `PriceWindow` only schedules **when** a price row is effective — pricing defines **no** promotional/trial window kind (pricing ADR-0003, design/07: states are `scheduled\|active\|expired\|cancelled`), so trial offers are **not** expressible as windows; trials are plan **phases** (pricing D-15). Completes the removal SEAMS SUB-P3 records (2026-07-29 cross-gear review, pricing D-66). Linkage/authoring and window scheduling/activation in the **Pricing gear** ([pricing PRD](../../pricing/docs/PRD.md)). |
| **prorationBasis** | Day-count convention (`calendar_days_actual`, `calendar_days_30`, `by_second`, `whole_unit`, or `none`) applied to **all** mid-period proration of the recurring component; configured on the plan/price policy and frozen in `pricingSnapshotRef`. The **canonical enum is owned by the pricing gear** ([pricing `design/06`](../../pricing/docs/design/06-consumer-contracts.md)) and adopted **verbatim** here and by Rating — CI gate `pricing.contracts.enum_drift` (2026-07-28 cross-gear review fix: the `none` value was missing and ownership was misattributed to Rating, which had already conformed). Subscriptions only sets the change boundary/mode. |
| **billingAnchorPolicy** | Pricing-published per-row policy fixing how the recurring billing anchor is derived; the **canonical enum (K2, incl. the D-20 no-drift month-end clamp) is owned by the pricing gear and adopted verbatim** here — the same discipline as `prorationBasis`, CI gate `pricing.contracts.enum_drift` (SUB-D-27, 2026-08-01; SB1 resolved this gear's way — rating T-D-33). **This gear's emitter executes the math**: `[periodStart, periodEnd)` derives from the frozen policy with the clamp (a Jan-31 monthly anchor bills Feb-28/29 and returns to the 31st — never permanent drift); an anchor-altering plan change takes effect at the **next** period boundary; the K5 joint anchor fixture (this gear's period identity ≡ rating's calendar geometry) is a design-freeze gate. `billingAnchor` — the anchor *instant* — still derives from Contract terms at activation (design slice 01 §3.7), and for an uncontracted subscription from the platform defaults: the activation instant (`serviceActivatedAt`); the policy governs how boundaries fall from it. |
| **Regional template** | Jurisdiction-scoped set of default contract terms (notice minima, grace, cancellation rights) with **statutory minima** a contract term may not fall below; authored by the **contracts** gear (`gears/bss/contracts/docs/PRD.md` `fr-grace-regional`). |
| **Renewal notice** | Notification sent before auto-renewal at configurable intervals (default 30/14/7/1 days) for customer awareness; triggered here, delivered via Notifications (§6.5). |
| **Resource-affecting transition** | Any transition that changes entitlements, provisioned resources, or quota-bearing bindings; MUST pass the Policy Engine gate before commit (manifest §4.3, §6). |
| **Resource quota** | Entitlement limiting resource provisioning (e.g. max 10 VMs, max 5 TB total storage). |
| **Soft limit** | Usage threshold triggering warnings while allowing continued usage; overage MAY bill per plan policy; the crossing is detected by the platform `quota-enforcement` gear, not here (§6.9). |
| **Subscription pause** | Temporary pause preserving subscription state with no charges during the pause window — distinct from suspension (service-affecting). Modeled as the **`collectionPaused`** posture on `active` (§6.4, SUB-D-03). |
| **Subscription revision** | Monotonic `version` (and/or revision record) capturing **effective-dated** composition and commercial snapshot pointers after a committed transition. |
| **Trial period** | Time-limited free or reduced-cost period modeled as the leading plan phase (§6.1); runtime, conversion, expiry, and extension in §6.10. |
| **Usage attribution binding** | Effective-dated link from a provisioned resource or subject to the subscription component that provisioned it, born at provisioning confirmation and closed at deprovision; the source Rating uses to attribute usage records, which carry no commercial identity (§6.2, `fr-usage-attribution`). |
| **Usage quota** | Numeric entitlement defining maximum allowed usage (e.g. 100 GB storage, 1000 API calls/month); counted by the platform `quota-enforcement` gear against the limit License Manager provisions from the subscription's posture (§6.9). |

## 2. Architecture Alignment

| **Field** | **Value** |
|-----------|----------|
| **Applicable Manifest(s)** | BSS |
| **Relevant Chapters** | §4.3 Subscriptions and Entitlements; §4.1 Product and Service Catalog (SKU/Plan/snapshots); §4.2 Rating and Charging (aggregate ordering, tariff evaluation, Usage→charge); §4.4 Billing and Invoicing (recurring `BillableItem`, immutability); §4.6 Contracts and Agreements (defaults, renewals, overrides); §2.1.3 Multi-tenant semantics; §6 BSS↔OSS Interlocks (Policy Engine gate) |

> **Normative baseline**: this PRD MUST remain consistent with the BSS manifest: Subscriptions/Entitlements are **SoR** in BSS; **OSS** provisions resources; **Policy Engine** gates **resource-affecting** transitions fail-closed; transitions are **idempotent** on `(subscriptionId, idempotencyKey)`; event ordering is **within `(tenantId, aggregateId)`** with **`aggregateId = subscriptionId`** for subscription streams; recurring fees are **`BillableItem(kind=recurring)`** idempotent per **`(subscriptionId, billing period, lineKey)`** (per component — SUB-D-19); published financial documents remain **immutable** with corrections via **adjustments/credit/debit notes** (Billing). Detailed API/event schemas belong in **Design** artifacts.

**Manifest conformance.** This PRD matches the manifest for SoR boundaries and Policy-gated resource-affecting changes (§2.1.2, §4.3, §6); subscription **status** values and transitions, including **resume**; entities `PlanLink`, `AddOn`, `TransitionRequest`, `Entitlement`, and `version`; lifecycle producers/consumers in §4.3, including **`SubscriptionPlanChanged`**; recurring **`BillableItem`** idempotency per **`(subscriptionId, billing period, lineKey)`** (SUB-D-19) and posted-invoice immutability (§4.4); and contract renewal semantics from §4.6 (`Renewal`, `autoRenew`, terms, snapshots).

**Catalog/Rating decomposition (cross-gear ownership).** Manifest §4.1/§4.2 are decomposed into dedicated PRDs that this PRD **consumes by reference** and MUST NOT re-author. In this repository two of them are vendored as sibling gears (see §2.1 for the mapping):

| **Concern** | **Authoritative owner** | **This PRD's relationship** |
|-------------|-------------------------|------------------------------|
| Product, SKU, Category, Attribute, `PlanTier` **taxonomy**, `CatalogVersion`, publish | Catalog registry — the **products** gear (`gears/bss/products/docs/PRD.md`, vendored 2026-07-16) | Reads **published** SKUs / `CatalogVersion`; `PlanLink` / overlap key bind to published catalog keys |
| `Plan`, `Price`, `PriceWindow` linkage, bundle/add-on rules, billing descriptors | **Pricing gear** — [pricing PRD](../../pricing/docs/PRD.md) (§4.1) | `PlanLink` / `AddOn` reference these primitives; trial offers are catalog trial plans or a leading trial **phase** — pricing defines no promotional/trial window kind (§1.4 PriceWindow) |
| Tariff **evaluation semantics**: graduated/volume math, override hierarchy, coupons, FX, **proration math** (`prorationBasis`) | **Rating gear**, evaluation core — [rating PRD](../../rating/docs/PRD.md) (§4.2) | Subscriptions owns the plan-change **WHEN** + `changeMode`; the evaluation core consumes `(changeEffectiveAt, changeMode)` and owns the math |
| Usage → `RatedCharge` / `BillableItem` orchestration, dedup, partition ordering | **Rating gear**, operational pipeline — [rating PRD](../../rating/docs/PRD.md) (§4.2) | Rating reads subscription composition + `PlanTier` @ `t` from this PRD's read models |

**Manifest silence and deferrals** (MUST be resolved in manifest, Design, or explicit product rules — this PRD MUST NOT invent conflicting enums or global cardinality): **trials** are handled per §6.1 (attribute/composition on manifest statuses, not a `trial` status value), including the **commercial pattern** there. **Overlapping active subscriptions** use the **default cardinality** in §6.3 (the manifest does not fix global cardinality). **ASC 606** operational detail is outside the manifest; this PRD only states subscription-level traceability and snapshot hooks for Finance/Billing. Design documentation MUST close trial attribute/event naming, overlap **dimension** binding, and **Payments/Billing integration details** (PSP webhooks, dunning handoff payloads) consistent with the **grace ladder** in §6.5. **New acquisitions**: manifest §4.6.1 routes them through a fulfilled Order ("only a fulfilled Order spawns the Subscription"); this PRD adopts the order path as the caller of record for commercially initiated creates and keeps the direct `create` for system, migration and operator paths and for deployments running before the order path ships (§6.1 `fr-order-originated-create`; manifest alignment tracked in §15).

### 2.1 Terminology and Naming

| **Name** | **Usage** |
|----------|-----------|
| **Subscriptions** | Canonical name of this gear and its domain (manifest §4.3): the subscription commercial aggregate, its lifecycle engine, composition, renewal, events, and multi-tenant ownership. |
| **Pricing (Product Catalog)** | The sibling gear vendoring upstream `PRD-plan-price-modeling-202605281200` — SoR for `Plan`/`Price`/`PriceWindow`/`PriceOverlay`/`CatalogVersion` that `PlanLink`/`AddOn` reference ([pricing PRD](../../pricing/docs/PRD.md)). |
| **Rating** | The sibling gear vendoring upstream `PRD-tariffs-pricing-logic-202604011200` + `PRD-rating-engine-202604031200`, **consolidated into one gear** per rating [ADR-0002](../../rating/docs/ADR/0002-cpt-cf-bss-rating-adr-rating-gear-consolidation.md): the pure evaluation core (`rating-core`, successor of "PLAL"/"Tariffs") plus the operational pipeline. Upstream text referring to "Tariffs" and "Rating" as two PRDs reads onto this one gear. |
| **Catalog registry** | The **products** gear — `gears/bss/products/docs/PRD.md` (Product/SKU/Category/Attribute/`PlanTier` taxonomy/`CatalogVersion`); vendored 2026-07-16 from upstream PR #4177 (provenance in the doc). |
| **Orders** | The **orders-lifecycle** (order document and state SoR), **orders-workflow** (approval and fulfillment orchestration — the caller of record for commercially initiated creates) and **orders-changes** (change orders for increases) gears — `gears/bss/orders-*/docs/PRD.md`. |
| **Contracts** | The **contracts** gear — `gears/bss/contracts/docs/PRD.md` (first draft, 2026-08-17: §1–§6 written, no acceptance criteria yet). Authors renewal terms, the notice and grace ladders, regional templates, ramps and the booking/acceptance declaration this PRD consumes. |
| **Billing / Payments / Promotions** | Billing posting is the **ledger** gear (`gears/bss/ledger/docs/PRD.md`); invoicing, dunning, Payments and Promotions have no gear in this repository and are referenced by their upstream PRD names (§17). |

### 2.2 Predecessor PRDs and Scope Migration

`PRD-subscriptions-entitlements-202601120119` describes the **Subscriptions + Entitlements** module in breadth (PRD-0001 mapping, SLAs, recurring charges, trials, and entitlement enforcement at point of use). This PRD began upstream as its manifest-first **specialization** for the subscription aggregate — §4.3 **status** model and operations (including **resume**), **`TransitionRequest`** / **idempotency** / **ordering**, Policy Engine and OSS interlocks, effective-dated **`PlanLink`** / **`AddOn`** and subscription **versioning**, recurring **`BillableItem`** rules aligned to §4.4, Contract-linked **renewal** and **failed-renewal** boundaries, and multi-tenant **ownership** / **delegation** — and has since absorbed the predecessor entirely (below). Work and acceptance criteria for all of these topics trace **here** and to **Design**.

**Consolidation (2026-07-15, this repository).** The predecessor is **absorbed into this PRD**; in this repository it is superseded in full. Section map:

| Predecessor section | Fate in this PRD |
|---|---|
| Subscription lifecycle / recurring / auto-renewal / proration ACs | Superseded by §6.1–§6.8 and §12 (manifest-first wording wins; predecessor's `pending` state reads as `draft`) |
| Entitlement Management framework (flags, quotas, soft/hard limits, p95 < 100ms check API) | **Absorbed, then re-split 2026-10-02** → §6.9 keeps the posture decision and the issuance request; materialization = **License Manager**; checks = platform `license-enforcement`; counting and soft/hard limits = `quota-enforcement` |
| Trial Management (creation, conversion, expiry, extension) | **Absorbed** → §6.10 (runtime on the phase machinery; status rules stay §6.1) |
| Renewal notices (30/14/7/1), opt-out | **Absorbed** → §6.5 |
| Module SLA table / PRD-0001 SLAs | **Absorbed** → §7.1 (+ operational baselines) |
| UX consoles (admin, partner portal, self-service, entitlement dashboard) | **Absorbed** → §10/§11 |
| Reconciliation framework | **Absorbed** → §17.1 appendix |
| Committed usage tracking / true-up | **Superseded to owners**: commitment pools = Contracts SoR, true-up = rating (T-D-14); this gear keeps subscription-side hooks only |
| Usage credit pools / prepaid | **Superseded to owners**: prepaid credit grant = pricing D-43 (definition), balance/drawdown = Billing/Rating (GA-gated) |
| Renewal price lock / grandfathering | **Superseded to owner**: pricing `priceEligibility` + `cohort` generations (pricing ADR-0002) |
| Subscription bundling economics | **Superseded to owner**: pricing bundles (rev-share, itemization); here only parent/child lifecycle sync (§5.1, `p3`) |
| Plan migration tools | **Superseded to owner**: pricing lifecycle slice authors migration; Subscriptions executes `PlanMigrationScheduled` |
| Market Intelligence / competitor matrix | Dropped; the vendor gap analysis that superseded it has left the corpus too |
| Module integration mermaid/sequence diagrams | Superseded by §4 diagrams; sequence depth → `DESIGN-subscriptions-*` |

**Canonical home**: this PRD (in `gears-rust`) is the single source of truth for the subscriptions gear; it absorbs the predecessor `PRD-subscriptions-entitlements` (section map above). The upstream vhp-architecture copies are **legacy provenance, not maintained** — there is no drift-tracking or back-port obligation.

## 3. Actors

### 3.1 Human Actors

#### Partner Admin

**ID**: `cpt-cf-bss-subscriptions-actor-partner-admin`

**Role**: Operates customer subscriptions across the reseller hierarchy — suspend/resume/cancel, plan changes, renewal intervention — without engineering involvement.
**Needs**: Subscription admin console, `TransitionRequest` tracking, delegation-proof-backed cross-tenant actions, auditable outcomes.

#### Customer (Self-Service)

**ID**: `cpt-cf-bss-subscriptions-actor-customer`

**Role**: Buys, changes, and cancels own subscriptions; previews commercial impact before confirming a change.
**Needs**: Plan-change wizard with proration preview, effective-timing choice (immediate / next-cycle / end-of-term), clear rejection reasons.

#### Finance Analyst

**ID**: `cpt-cf-bss-subscriptions-actor-finance-analyst`

**Role**: Monitors renewals and dunning exposure; audits lifecycle-to-invoice lineage for compliance.
**Needs**: Failing-renewal filters, audit-trail export, snapshot-linked charge traceability.

#### Product Manager (Entitlement Admin)

**ID**: `cpt-cf-bss-subscriptions-actor-product-manager`

**Role**: Configures entitlement templates on plans (via the catalog grant sets) and monitors quota usage, limits, and compliance across subscriptions.
**Needs**: Entitlement dashboard — templates, quota monitoring, flag management, compliance reports and export.

#### Platform Operator

**ID**: `cpt-cf-bss-subscriptions-actor-platform-operator`

**Role**: Owns governed subscription lifecycles so revenue, entitlements, and invoices stay consistent across tenants and channels.
**Needs**: State-machine observability, DLQ/replay for failed transitions, approval workflow for high-risk transitions (transfer).

### 3.2 System Actors

#### Pricing (Product Catalog)

**ID**: `cpt-cf-bss-subscriptions-actor-pricing`

**Role**: SoR for `Plan`/`Price`/`PriceWindow` linkage, bundle/add-on rules, billing descriptors, and publish/sellability gates that `PlanLink`/`AddOn` resolve against; trial offers are catalog trial plans or a leading trial phase ([pricing PRD](../../pricing/docs/PRD.md)).

#### Catalog Registry (Product & SKU)

**ID**: `cpt-cf-bss-subscriptions-actor-catalog-registry`

**Role**: SoR for published `skuId`, `PlanTier` taxonomy, `CatalogVersion`; the overlap key (`catalogSubscriptionProductKey`) binds to its published keys (the **products** gear, `gears/bss/products/docs/PRD.md`). The registry does not yet expose that key (SEAMS SUB-G1; also asked by Orders Lifecycle).

#### Rating (Evaluation Core + Pipeline)

**ID**: `cpt-cf-bss-subscriptions-actor-rating`

**Role**: Reads subscription composition, effective `PlanLink`, `PlanTier`, and active **plan phase** at `t` via read models; consumes plan-change `(changeEffectiveAt, changeMode)` for proration math; slices usage at the same boundary; owns Usage → `RatedCharge` / `BillableItem` orchestration ([rating PRD](../../rating/docs/PRD.md)).

#### Billing & Invoicing

**ID**: `cpt-cf-bss-subscriptions-actor-billing`

**Role**: Ingests recurring `BillableItem`s; aligns periods/proration artifacts; posts immutable invoices; executes adjustments/credit/debit notes; runs dunning with Payments.

#### Contracts & Agreements

**ID**: `cpt-cf-bss-subscriptions-actor-contracts`

**Role**: SoR for signed terms, `Renewal` (`autoRenew`, term windows, notice ladder), grace ladder and regional templates, ramps, `PriceOverride` windows, the booking instant and the acceptance-required declaration; supplies them via events and read models (the **contracts** gear, `gears/bss/contracts/docs/PRD.md`). Optional per subscription: where none is bound, the platform defaults govern (§6.5).

#### Orders (Lifecycle + Workflow + Change Orders)

**ID**: `cpt-cf-bss-subscriptions-actor-orders`

**Role**: Orders Workflow submits the order path's intents — draft-create and activation per fulfilled `new_sale` line, draft-void and activated-cancel as compensation, one change intent per change order — each carrying the order identity envelope, and consumes the per-intent confirmation or failure. Orders Lifecycle reads overlap occupancy and composition for its submit and delta gates. Orders never mutates subscription state directly (orders-lifecycle §6.4 R3, R5).

#### License Manager

**ID**: `cpt-cf-bss-subscriptions-actor-license-manager`

**Role**: The BSS gear that materializes the entitlement posture this gear decides: on an idempotent issuance request keyed by the commercial event it issues, renews, replaces, suspends, reinstates and revokes the licenses, packs and limits that back a subscription — packs into the platform `license-enforcement` gear and limits into `quota-enforcement` for platform scopes, signed license artifacts for installed products outside the platform — and reports license state and findings back (upstream `PRD-license-manager-202610021000`). It never decides when a subscription is due, renewed or cancelled.

#### Policy Engine

**ID**: `cpt-cf-bss-subscriptions-actor-policy-engine`

**Role**: Gates **resource-affecting** transitions fail-closed before commit; returns allow/deny + `reasonCodes` (manifest §6).

#### OSS Provisioning

**ID**: `cpt-cf-bss-subscriptions-actor-oss-provisioning`

**Role**: Executes provision/deprovision/pause work orders confirmed by events; BSS never mutates OSS resource topology directly (manifest §2.1.2).

#### Payments (PSP)

**ID**: `cpt-cf-bss-subscriptions-actor-payments`

**Role**: Supplies payment pre-check and retry-exhaustion signals consumed by the renewal/grace ladder; payment capture and PSP behavior stay out of scope (§4.5).

#### AMS / OSS (Tenant Identity)

**ID**: `cpt-cf-bss-subscriptions-actor-ams`

**Role**: SoR for tenant identity and topology (`resourceTenantId` references, account/OrgTier context); Subscriptions references, never invents, tenant topology.

#### Analytics / DWH

**ID**: `cpt-cf-bss-subscriptions-actor-analytics`

**Role**: Consumes lifecycle facts.

## 4. Operational Concept & Environment

```text
┌─────────────────────────────────────────────────────────────────────────┐
│ Common Core: IdP, API Gateway (Inbound), Events & Audit, Correlation IDs │
└─────────────────────────────────────────────────────────────────────────┘
         │                                    │
         ▼                                    ▼
┌─────────────────┐    Policy Engine gate     ┌──────────────────────────┐
│  Contracts      │ ──defaults/snapshots──▶   │  Subscriptions (SoR)      │
│  (SoR) §4.6     │    ContractSigned/...     │  §4.3                     │
└─────────────────┘                           │  State machine, PlanLink, │
         │                                    │  Entitlement issue/revoke │
         │                                    └───────────┬───────────────┘
         │                                                │
         │              ┌─────────────────────────────────┼──────────────────┐
         │              │                                 │                  │
         ▼              ▼                                 ▼                  ▼
┌──────────────┐  ┌──────────────┐                 ┌──────────────┐   ┌──────────────┐
│ Catalog §4.1 │  │ OSS Provision│                 │ Rating §4.2  │   │ Billing §4.4 │
│ sku/plan/ref │  │ (execute)    │                 │ usage+rules  │   │ invoice post │
└──────────────┘  └──────────────┘                 └──────────────┘   └──────────────┘
```

**End-to-end lifecycle (value stream)**

```text
[Contract, optional] ─ defaults / eligibility ─┐
                                               ▼
Order (fulfilled, Orders Workflow) ──▶ create(draft) ×N ──▶ activate ×N ──▶ [active ── suspended loop] ──▶ cancel ──▶ archived
   │  wave 1: every line in draft        │  wave 2        │                              │
   │  compensation: void (draft)         │                └──▶ PlanLink/AddOn updates ──┘
   │  or cancel (order_compensation)     │                └──▶ recurring period fact ──▶ Rating (price) ──▶ Billing ──▶ Invoice(posted)
   │                                     └──▶ one-time fact at activation ──▶ Billing
Change order ──▶ one change intent (all-or-nothing) ──▶ quantity / component increase on the running subscription
OSS Usage ──▶ Rating (attributes via this gear's usage attribution read) ──▶ BillableItem(usage) ──▶ Billing
```

`ContractSigned` supplies defaults and eligibility only — it does not create a subscription (manifest §4.6). The direct `create` remains for system, migration and operator paths (§6.1).

**Policy-gated transition (condensed)**

```text
Client ─▶ API GW ─▶ Subscriptions: validate + TransitionRequest
                      │
                      ├─▶ Policy Engine: pre-check (resource-affecting)
                      │        └─ deny → reasonCode (fail-closed)
                      ├─▶ OSS: provision/deprovision (confirm)
                      └─▶ Entitlements issue/revoke → CloudEvents → Billing/Rating/Analytics
```

**Inbound (Subscriptions consumes)**

- **AMS/OSS**: tenant identity, `resourceTenantId` topology references (read-only for the BSS SoR split).
- **Catalog registry**: published `skuId`, `PlanTier` taxonomy, `CatalogVersion` for eligibility.
- **Pricing gear**: published `planId`, `PriceWindow` linkage / price snapshot refs that `PlanLink` / `AddOn` resolve against.
- **Contracts**: signed terms, `Renewal`, notice and grace ladders, regional templates, ramps, `PriceOverride` windows, booking instant and acceptance declaration (events + read models); absent a bound contract, the platform defaults (§6.5).
- **Orders Workflow**: draft-create, activation, draft-void and activated-cancel intents per order line; one change intent per change order; each with the order identity envelope (§6.1, §6.3, §9.2).
- **Policy Engine**: allow/deny + `reasonCodes` for resource-affecting transitions.

**Outbound (Subscriptions produces)**

- **OSS Provisioning**: work orders confirmed by events (manifest flows).
- **Billing**: `BillableItemCreated(kind=recurring)` — the money-free period fact with stable catalog refs + `pricingSnapshotRef`; the rating gear prices it before Billing posts (SUB-D-07, §6.8; per the §4.1–4.2 contract).
- **Rating** (indirect): composition, effective `PlanLink`, `PlanTier`, and active **plan phase** at `t` via read models; plan-change `(changeEffectiveAt, changeMode)` consumed for proration math; the **usage attribution read** that maps a usage record's resource or subject to its subscription component at `t` — usage arrives from the Usage Collector with no commercial identity (§6.2 `fr-usage-attribution`).
- **Orders**: per-intent confirmations and failures echoing the identity envelope; overlap occupancy and composition reads for the order gates (§9.1, §9.2).
- **Analytics/DWH**: lifecycle facts.

### 4.1 Module-Specific Environment Constraints

- All effective dating, anchors, and boundaries are **UTC**; events are **CloudEvents 1.0**, tenant-scoped, minimal PII.
- Every mutating request is **idempotent** on `(subscriptionId, idempotencyKey)` — `create`, which has no `subscriptionId` yet, on `(orderingTenantId, operation = create, idempotencyKey)`; consumers preserve ordering within `(tenantId, aggregateId = subscriptionId)`.
- **Resource-affecting** transitions never commit without a Policy Engine pre-check (fail-closed) and, where required, OSS provisioning confirmation.
- BSS MUST NOT mutate OSS resource topology; Subscriptions **requests** changes via Policy-gated workflows only.

## 5. Scope

### 5.1 In Scope

| **Feature** | **Priority** | **Notes** |
|-------------|--------------|-----------|
| Lifecycle state machine (manifest-aligned states + transitions) | `p1` | §6.1; terminality rules |
| Subscription versioning & effective-dated `PlanLink` / `AddOn` | `p1` | Aligns Rating/Billing to the same SKU/Plan set over time (§6.2) |
| Plan change (upgrade/downgrade) with proration policy hooks | `p1` | Triggers Billing alignment; no posted-invoice mutation (§6.3) |
| Suspension & reactivation (Policy + OSS paths) | `p1` | Entitlement revoke/restore per §4.3 (§6.4, §6.9) |
| Entitlement posture and the issuance request to License Manager on subscription transitions | `p1` | Driven by activate/suspend/resume/cancel, renewal, phase boundaries and composition-changing transitions; materialization, checks and counting are the platform licensing gears' (§6.9, §2.2) |
| Renewal (auto/manual) with Contract linkage | `p1` | Consumes `ContractRenewed` / renewal terms where applicable (§6.5) |
| Event model (manifest producers + correlation) | `p1` | CloudEvents 1.0; ordering invariant (§6.7) |
| Recurring `BillableItem` emission to Billing | `p1` | Idempotent `(subscriptionId, billing period, lineKey)` — per component, SUB-D-19 (§6.8) |
| Multi-tenant ownership & delegation proofs | `p1` | AMS/OSS identity backbone by reference (§6.6) |
| License state and findings from License Manager on the subscription read model | `p1` | §6.9 `fr-entitlement-license-state`; replaces the former check contract and quota tracking rows (re-split 2026-10-02) |
| Trial runtime & conversion (create, auto-convert, early `convertTrial`, expire, extension) | `p1` | §6.10; rides the pricing phase machinery (D-19/D-41) |
| Scheduled lifecycle intents (cancel at term end / at date; resume-at) | `p1` | §6.1 pending intents + renewal-job interaction (SUB-D-01) |
| Seat/quantity change transition (`updateQuantity`) | `p1` | §6.3 envelope + provenance for pricing D-18 seat counts (SUB-D-02) |
| Order-originated acquisition: two-phase create/activate, order reference, start at the activate commit, compensation | `p1` | §6.1; Orders Workflow is the caller of record for commercially initiated creates (SEAMS SUB-O1/O2/O3/O10) |
| Order change intent: transactional multi-item increase with an expected revision | `p1` | §6.3 (Change Orders CHG-S1…S5) |
| Overlap occupancy read + atomic enforcement at entry into `active` | `p1` | §6.3 (SEAMS SUB-O5, amended) |
| Caller intent protocol: resubmit/in-flight signal, withdrawal, status read, identity echo, correlation propagation | `p1` | §6.1 (SEAMS SUB-O11…O14, SUB-O16) |
| Platform defaults for uncontracted subscriptions; explicit contract binding | `p1` | §6.5 |
| Usage attribution binding + as-of read for Rating | `p1` | §6.2 |
| Resource and data disposition on suspend and cancel: retention window, export, purge | `p1` | §6.11 |
| Subscription record retention, legal hold, personal-data erasure | `p1` | §6.11 |
| Co-termination: add-ons, change-order items, co-term date | `p1` | §6.5 |
| Term source and evergreen semantics | `p1` | §6.5 |
| Import of live subscriptions from a legacy billing system | `p1` | §6.1 |
| API surface (control plane): create, read, update metadata, transitions, cancel | `p2` | Business verbs only in PRD; idempotency + optimistic concurrency (§9.1) |
| Backdated & overlapping subscription rules | `p2` | §6.3; AC 6/8 |
| Failed renewal / dunning handoff | `p2` | Subscriptions state + Billing/Payments boundaries (§6.5) |
| ASC 606 hooks (PO tags, SSP snapshot refs on recurring lines) | `p2` | Downstream Finance/Billing |
| Renewal notices (configurable, default 30/14/7/1) + opt-out | `p2` | §6.5; delivery via Notifications |
| Billing-only pause (`collectionPaused` on `active`) | `p2` | §6.4 posture (SUB-D-03); billing-cycle mechanics open in §15 |
| Bulk subscription operations (batch create/update/cancel, async) | `p2` | Partner mass operations; batching in Design |
| Operator UX / approvals for high-risk transitions | `p3` | Ownership transfer cross-ref manifest §4.11 |
| Bundled-subscription lifecycle sync (parent/child) | `p3` | Bundle economics = pricing gear; here only synchronized lifecycle |
| Subscription analytics events (MRR/ARR, churn signals) | `p3` | Facts for Analytics/DWH (§6.7) |

### 5.2 Out of Scope

- **Proto/OpenAPI schemas**, error code taxonomies, DB DDL — **Design**.
- **OSS resource topology** mutations by BSS — **forbidden** (manifest §2.1.2); Subscriptions **requests** changes via Policy-gated workflows.
- **Entitlement materialization, point-of-use checks and quota counting** — License Manager (issuance of licenses, packs and limits), `license-resolver` / `license-enforcement` (checks) and `quota-enforcement` (counting, soft/hard limits); re-split 2026-10-02, §6.9.
- **Full revenue recognition** and **subledger journals** — Finance/Billing; this PRD supplies **subscription-level** traceability and snapshot refs.
- **Payment capture** and **PSP** behavior — manifest §4.5; the subscription MAY react to **payment failure** events if defined in Design.
- **`trial` as `Subscription.status`** and a dedicated **`trial` → `active` | `cancelled`** lifecycle in the §4.3 state machine — **out of scope** until the manifest enum is amended; until then trials are **attributes/composition** only (§6.1).
- **Notification delivery channels and campaign content** (renewal notices, trial expiry, win-back messaging) — Notifications/Comms; this PRD fixes the triggers and intervals only (§6.5, §6.10).
- **Enforcement execution at the point of use** — OSS enforces (allow/block/degrade) against this gear's check contract and quota state (§6.9); the enforcement action itself and graceful-degradation behavior mid-request are OSS/Design concerns (§15).
- **UI implementation** of the consoles/portals in §11 — Presentation layer; this PRD fixes the operations they invoke.
- **Order capture, approval and fulfillment orchestration** — the Orders gears; this PRD fixes only what Subscriptions accepts from them and returns (§6.1, §6.3, §9.2). Order-driven **decreases, removals and plan changes** are not specified by Change Orders yet; until they are, those changes use the direct operations.

## 6. Functional Requirements

> **Content boundary**: FRs define WHAT the lifecycle engine must guarantee, not data models or APIs. Concrete schemas, event attribute matrices, REST mappings, timers, and error taxonomies are owned by the corresponding DESIGN (`DESIGN-subscriptions-*/`). Proration **math** and tariff evaluation are owned by the Rating gear; posting and invoice immutability enforcement by Billing.

### 6.1 Lifecycle State Machine

#### Status enum and terminality

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-status-enum`

Manifest `Subscription.status` is **`draft` | `active` | `suspended` | `cancelled` | `archived`**. The allowed path is **Draft → Active → Suspended → Cancelled → Archived**; **terminal states are immutable** except **archival** progression (`cancelled` → `archived`; no commercial rebirth without a new subscription). **Resume** (`suspended` → `active`) is explicitly supported and MUST be modeled as the reverse edge (the manifest lists operations, not every arrow in one line). **Void** (`draft` → `cancelled`) is likewise supported so abandoned drafts are exitable through the normal commit path — it is **not resource-affecting** (nothing is provisioned yet, no OSS leg) and accepts `cancelMode = immediate` only (SUB-D-11; an optional draft-retention TTL that submits the void is a Product knob, §15).

```text
        ┌──────────┐
        │  draft   │
        └──┬────┬──┘
           │    │ cancel (void — SUB-D-11)
  activate │    └─────────────────────────────────────────┐
  (Policy+OSS+entitlements)                               │
           ▼                                              │
        ┌──────────┐      suspend       ┌───────────┐     │
        │  active  │───────────────────▶│ suspended │     │
        │          │◀───────────────────│           │     │
        └────┬─────┘   resume (Policy)  └─────┬─────┘     │
             │ cancel                         │ cancel    │
             │ (Policy+OSS deprovision)       │           │
             ▼                                ▼           ▼
        ┌─────────────────────────────────────────────────────┐
        │                      cancelled                      │
        └──────────────────────────┬──────────────────────────┘
                                   │ archive (retention / legal)
                                   ▼
                            ┌──────────┐
                            │ archived │  (terminal)
                            └──────────┘
```

**Rationale**: A closed, manifest-aligned enum with explicit terminality is what makes lifecycle audit and replay possible.

**Actors**: `cpt-cf-bss-subscriptions-actor-platform-operator`

#### Normative transitions and guards

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-transitions-guards`

| **From** | **To** | **Trigger (verb)** | **Guard (summary)** |
|----------|--------|--------------------|----------------------|
| draft | active | activate | Policy allow + OSS provision confirm + entitlements issued (manifest §4.3); target resolves to a **published** plan passing the adopted pricing sellability gate — incl. the D-80 coverage horizon and the D-94 **full conjunction over every scope key the purchase binds** on the bound `(currency, region)`, one failing key blocks the sale (SEAMS SUB-P5; carried into this PRD 2026-08-01, wave-3 review #17; same gate on `create`/`changePlan`) |
| draft | cancelled | cancel (void) | Not resource-affecting (nothing provisioned); `cancelMode = immediate` only; audited (SUB-D-11) |
| active | suspended | suspend | Policy + coordinate the OSS pause; data-bearing resources are never deleted on suspend (§6.11) |
| suspended | active | resume | Policy allow; where the suspension was grace-driven, the blocking payment failure MUST be resolved first (§6.5) |
| active | cancelled | cancel | Policy + OSS deprovision + entitlement revocation |
| suspended | cancelled | cancel | Same as cancel from non-active where permitted |
| cancelled | archived | archive | Retention/governance; no commercial rebirth without a new subscription |

Every **resource-affecting** transition MUST pass the Policy Engine pre-check before commit; on deny the subscription state MUST NOT change (AC 1).

**Rationale**: Guards are the contract that keeps entitlements, provisioned resources, and commercial state consistent.

**Actors**: `cpt-cf-bss-subscriptions-actor-policy-engine`, `cpt-cf-bss-subscriptions-actor-oss-provisioning`

#### Trials are not a status

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-trials-not-a-status`

A commercial **trial** is **not** a `Subscription.status` value. Manifest §4.3 lists **`draft` | `active` | `suspended` | `cancelled` | `archived`** only; this PRD MUST NOT add a **`trial`** state or edges such as **`trial` → `active`** / **`trial` → `cancelled`** unless the BSS manifest enum is extended first. Trial periods MUST be expressed with **attributes** and/or **effective-dated composition** (trial **plan/SKU** via `PlanLink`, contract or subscription flags, Catalog trial offers) while the subscription occupies a manifest status — commonly **`draft`** before first paid activation and/or **`active`** when service is delivered under trial commercial rules. Where a plan defines time-bounded phases, the trial is modeled as the leading **plan phase** (`trial` → `interim`/`evergreen`; §1.4 Plan phase); **phase structure is Pricing SoR** (authored and published per plan revision), **phase runtime is Subscriptions SoR** (entry, conversion, and the active phase at `t`), and the Rating gear resolves the active phase at `t` for pricing. End-of-trial without conversion uses normal transitions and composition changes (**cancel**, **changePlan**, or attribute update) without a dedicated trial terminal **status**.

**Rationale**: Keeping trials out of the status enum preserves manifest conformance and keeps the state machine closed.

**Actors**: `cpt-cf-bss-subscriptions-actor-rating`

#### Trial commercial pattern

- [ ] `p2` - **ID**: `cpt-cf-bss-subscriptions-fr-trial-commercial-pattern`

**Catalog** is authoritative for the **trial sellable definition** (trial plan/SKU, a trial **phase** on the plan, or a time-bounded **`PlanLink`** to a trial offer — the third enumeration member, "promotional `PriceWindow`", is removed: pricing has no such window kind, SEAMS SUB-P3, 2026-07-29, pricing D-66). **Contract** MAY carry **legal and commercial trial clauses** (notice, conversion, caps) where a contract is bound; no Contracts artifact authors such clauses today (the contracts gear's §6 has none), so trial terms default to the Catalog definition plus the platform defaults (§6.5). **Subscription** persists **evaluated** trial state as **attributes** plus effective **`PlanLink`** / snapshot pointers so Rating/Billing stay deterministic. **Attribute-only** trials without a Catalog-managed offer are **not permitted**: with no Contract-authored trial terms they would have no auditable commercial basis. Trial **runtime, conversion, expiry, and extension** are normative in §6.10 (absorbed from the predecessor — §2.2); **Design** defines concrete fields and optional trial-specific events while preserving the status enum.

**Rationale**: A Catalog-first trial definition keeps trial economics reproducible and auditable without a status-machine fork.

**Actors**: `cpt-cf-bss-subscriptions-actor-pricing`, `cpt-cf-bss-subscriptions-actor-contracts`

#### TransitionRequest

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-transition-request`

All mutating operations SHOULD be modeled as **`TransitionRequest`** with `type ∈ { activate, suspend, resume, cancel, archive, changePlan, addAddOn, removeAddOn, updateQuantity, changeComposition, convertTrial, transfer, renew, unschedule, pauseCollection, resumeCollection, confirmAcceptance, extendTrial }`, `idempotencyKey`, `status ∈ { pending, approved, applied, failed }`. A request withdrawn before it is applied (§6.1 `fr-intent-protocol`) terminates as `failed` with the machine-readable reason `withdrawn` — no new status value. The base list is manifest §4.3; **`updateQuantity`** (§6.3), **`changeComposition`** (the order change intent, §6.3 `fr-composite-change`), **`convertTrial`** (§6.10), and the SUB-D-08 completion set — **`renew`** (manual renewal, §6.5), **`unschedule`** (voids a pending scheduled intent, §6.1/AC 22), **`pauseCollection`**/**`resumeCollection`** (the §6.4 posture window), **`confirmAcceptance`** (§6.1 activation instants), **`extendTrial`** (§6.10, approval-gated), **`archive`** (the `cancelled → archived` retention edge, retention-job-submitted — 2026-07-28 review fix: Design slice 01 had it, this list had missed it) — are this-PRD extensions pending manifest alignment (§15). Without them, mutations the FRs already mandate would bypass the single commit path. High-risk types (e.g. **transfer**, **extendTrial**) require **Approval** records (manifest §4.3, §4.11). Duplicate `(subscriptionId, idempotencyKey)` MUST result in exactly **one** durable effect.

**Rationale**: A uniform request envelope gives idempotency, approval hooks, and audit one shape.

**Actors**: `cpt-cf-bss-subscriptions-actor-partner-admin`, `cpt-cf-bss-subscriptions-actor-platform-operator`

#### Order-originated acquisition (two-phase create and activate)

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-order-originated-create`

Commercially initiated acquisitions arrive from **Orders Workflow** as per-line intents in two waves ([orders-lifecycle PRD](../../orders-lifecycle/docs/PRD.md) `fr-order-atomic-fulfillment`, [orders-workflow PRD](../../orders-workflow/docs/PRD.md) `fr-owf-provisioning-intent`). This gear MUST honour that shape:

1. **Create in `draft`, activate separately.** `create` and `activate` MUST remain two externally callable operations; the order path MUST NOT be served by a create-and-activate shortcut. A draft is not resource-affecting: no Policy gate, no OSS leg, no billable facts (SUB-D-11). Order-level atomicity rests on this pair (SEAMS SUB-O3).
2. **Order reference.** `create` MUST accept an optional order reference — `orderId`, `orderVersion`, `orderLineId`, and the order's external reference where present — persist it on the aggregate, and carry it on that subscription's lifecycle events and billable facts (§6.8). A subscription created outside the order path carries none, so the two are distinguishable (SEAMS SUB-O2).
3. **Dedup key.** `create` MUST accept a caller-supplied idempotency key and honour it as the constructor dedup key (§4.1). On the order path the caller derives it from `orderId`, `orderVersion`, line and wave, so an intent from a later `orderVersion` is a different operation and is never absorbed as a duplicate of a superseded version.
4. **Composition at create.** One order line creates exactly **one** subscription carrying the line's quantity; a bundle plan is one line and one subscription. Add-ons selected on the line ([orders-changes PRD](../../orders-changes/docs/PRD.md) `fr-chg-addon-selection`) MUST arrive with `create`, so the draft holds the full composition, validated against the plan's published add-on rules (required, eligible, min/max/step).
5. **Start instant.** The subscription start is `serviceActivatedAt`, stamped at the `activate` commit (SUB-D-05). It MUST NOT be taken from any date carried on the order, nor supplied by the caller — the import mode (`fr-import`) is the only exception. The order path dispatches activation only once expected fulfillment time is reached, so the commit instant is the actual activation instant; billing, entitlements and the term are never backdated to an earlier quoted service date (SEAMS SUB-O10).
6. **Price and market.** The line's catalog price pin is the catalog segment of the subscription's snapshot. This gear binds `(currency, region)` from the payer's commercial profile at activation (§6.2 Snapshot discipline) and MUST reject activation with a machine-readable **market-divergence** reason where that binding differs from the currency and region the line was priced in.
7. **At-sale money** is emitted at activation, never in `draft` (§9.2 one-time lane).
8. **Compensation.** Before activation the order path compensates with the draft void (§6.1 Status enum); after activation, with a `cancel` carrying reason **`order_compensation`** (`fr-cancel-reasons`).
9. **Direct path.** `create` without an order reference remains available for system, migration and operator paths, and for deployments running before the order path ships. Once the order path is live in a deployment, commercially initiated creates MUST carry an order reference; whether a deployment enforces that is a policy setting, not a separate operation.

**Rationale**: The order is the auditable record of a commercial acquisition (price pin, approval, booking), and all-or-nothing fulfillment across lines is cheap only because nothing is live or billed before activation. Without the reference, the dedup rule and a commit-stamped start, the order path either loses provenance, double-creates under retry, or backdates service.

**Actors**: `cpt-cf-bss-subscriptions-actor-orders`, `cpt-cf-bss-subscriptions-actor-platform-operator`

#### Cancellation reasons

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-cancel-reasons`

Every `cancel` MUST carry one reason from a **closed, versioned** set: `customer`, `operator`, `term_expired`, `nonpayment_exhausted`, `saga_superseded`, `order_compensation`. `order_compensation` is used only by the order path to roll back a subscription activated by an order whose fulfillment then failed. It is **outside** the early-termination class and derives neither an early-termination fee nor an unused-portion credit (SUB-D-25 scopes that class to `customer`/`operator`). Operational compensation does not retract posted at-sale money: a one-time fact already emitted stays posted, and its reversal is a Billing-chain artifact (credit note / adjustment) triggered by the compensation cancel. A new reason value is a breaking change for consumers, so the set MUST be fixed before Billing consumes `SubscriptionCancelled` (SEAMS SUB-O1).

**Rationale**: An order rollback is neither a customer-initiated early termination nor a plan-change supersession; reusing either value misstates the audit trail and derives money that is not owed.

**Actors**: `cpt-cf-bss-subscriptions-actor-orders`, `cpt-cf-bss-subscriptions-actor-billing`

#### Caller intent protocol (acceptance, withdrawal, status, identity)

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-intent-protocol`

For callers that orchestrate transitions asynchronously — Orders Workflow first, any system actor thereafter — this gear MUST provide:

1. **Resubmit and in-flight signal.** A resubmit with the same idempotency key MUST return the existing request and its current status (`pending`, `approved`, or terminal) — never a second effect and never a generic error — so the caller reads "already accepted" machine-readably (SEAMS SUB-O11; AC 2). A *different* resource-affecting request against a subscription with an in-flight resource transition is rejected `transition_in_flight` (Design slice 01 §3.6 in-flight single-writer rule).
2. **Withdrawal.** A request that is accepted but not yet applied MUST be withdrawable by its submitter, and terminates as `failed` with reason `withdrawn`. Where no OSS leg has been dispatched, the withdrawal has no effect on the subscription. Where a work order is already outstanding (an OSS-blocking edge in `approved`), withdrawal follows the superseding-action rule of Design slice 01 §3.6: the work order is idempotently cancelled with a compensating deprovision and audit, and the request terminates only after that completes. An applied request cannot be withdrawn; it is superseded by a compensating transition (SEAMS SUB-O12).
3. **Status read.** A request's status MUST be readable by its `TransitionRequest` identifier and, for order-originated requests, by `orderId` + `orderVersion` + `orderLineId` + wave, including after the idempotency key's lifetime has passed (SEAMS SUB-O13).
4. **Identity echo.** Every confirmation and failure MUST echo the envelope received on the request: the order reference where present, wave, idempotency key, the caller's `correlationId`, the caller's opaque binding reference, and the asserting principal. The binding reference is opaque here and MUST NOT be interpreted as order state (SEAMS SUB-O16).
5. **Correlation propagation.** The caller's `correlationId` MUST travel on the Policy Engine and OSS legs this gear drives for that request (SEAMS SUB-O14).

**Rationale**: A caller that cannot tell "accepted" from "lost", cannot abandon an accepted request, and cannot look one up after its key ages out can only retry blindly — the inference that produces double provisioning.

**Actors**: `cpt-cf-bss-subscriptions-actor-orders`, `cpt-cf-bss-subscriptions-actor-platform-operator`

#### Import of live subscriptions from a legacy billing system

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-import`

A service provider moving onto the platform brings live subscriptions from its previous billing system (for example WHMCS or HostBill). This gear MUST support an **import** creation mode, distinct from `create` + `activate`, that brings such a subscription in **as it already runs**:

1. **State as found.** An import creates the subscription directly in `active` or `suspended` — a creation edge outside the `draft → activate` pair, tracked for manifest alignment (§15) — carrying the original start, the current term window and term sequence, the billing anchor, the instant the legacy system has billed through, the composition (plan, add-ons, quantities), the price it is billed at (a catalog price or a contract override), and any trial or grace position. This is the one place where a start instant is supplied by the caller (§6.1 `fr-order-originated-create` item 5 does not apply).
2. **Adopt, never provision.** Existing resources are **adopted**: the import lists their references, they are validated against the OSS inventory, and they open the usage attribution binding (§6.2 `fr-usage-attribution`) from the import instant. No provisioning work order runs. Entitlements are issued to match the imported composition; the Policy Engine pre-check applies as for activation.
3. **No double billing.** No one-time or setup fact is emitted for an imported component, and the first recurring fact starts at the legacy billed-through instant; that instant also seeds the backdating guard (§6.3 `fr-backdated-changes`).
4. **Provenance and idempotency.** The subscription records `imported` provenance with the source system and its legacy identifier; an import is idempotent on `(source system, legacy identifier)`.
5. **Batch.** Imports run in batches with a per-item outcome; one failing item never blocks the rest.
6. **Who.** Import is a system or operator operation, never customer-facing, and every import is audited.

**Rationale**: Every service-provider onboarding is a migration of existing customers. Routed through `create` + `activate`, an imported subscription would re-provision running resources, bill the setup fee again and restart its term.

**Actors**: `cpt-cf-bss-subscriptions-actor-platform-operator`, `cpt-cf-bss-subscriptions-actor-oss-provisioning`

#### Scheduled lifecycle intents

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-scheduled-intents`

`cancel` MUST accept the same effective-timing vocabulary plan changes already use (§6.3): **`cancelMode ∈ { immediate, end_of_term, at(date) }`**; `suspend` MAY carry a **`resumeAt`** instant (scheduled resume). A non-immediate intent is stored as a **pending intent on the aggregate**: visible to the renewal job — a pending **end-of-term cancel MUST suppress renewal attempts and next-term recurring emission** (§6.5) — auditable, and **un-schedulable** (cancellable) until it takes effect; scheduling and un-scheduling both emit events. At the effective instant the normal transition executes with its full guard set (§6.1). (Decision SUB-D-01.)

**Rationale**: "Cancel at period end" is the most common self-service intent; unless it lives on the aggregate, the renewal job, Billing, and audit cannot see it.

**Actors**: `cpt-cf-bss-subscriptions-actor-customer`, `cpt-cf-bss-subscriptions-actor-partner-admin`

#### Commercial activation instants (booking / service / acceptance)

- [ ] `p2` - **ID**: `cpt-cf-bss-subscriptions-fr-activation-instants`

The aggregate MUST record three commercial instants as attributes/evaluated fields — **`contractEffectiveAt`** (booking; referenced from the Contract), **`serviceActivatedAt`** (stamped at the `activate` commit), and **`customerAcceptedAt`** (stamped by an optional **acceptance confirmation** operation where the Contract carries acceptance clauses; absent clauses ⇒ it equals service activation per the Contract default). For an **order-originated** subscription whose order recorded buyer acceptance (orders-lifecycle `OrderAcceptanceRecorded`), `customerAcceptedAt` MUST be taken from that record and no second confirmation is required; the order path and `confirmAcceptance` MUST NOT both stamp it. An uncontracted subscription (§6.5) has no `contractEffectiveAt`. **No new statuses**: pending-activation / pending-acceptance interim states are rejected — the manifest enum stays closed, `draft` covers the pre-activation window. All three instants MUST ride the lifecycle events and the ASC input hooks (§5.1); recognition semantics stay Finance/Billing. (Decision SUB-D-05.)

**Rationale**: Enterprise/channel deals with acceptance clauses need booking, service, and acceptance instants for correct revenue timing — a single `activatedAt` collapses them.

**Actors**: `cpt-cf-bss-subscriptions-actor-contracts`, `cpt-cf-bss-subscriptions-actor-finance-analyst`

### 6.2 Versioning and Effective-Dated Composition

#### Monotonic revision

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-monotonic-version`

Each committed transition that changes commercial meaning MUST increment `version` (or append an immutable revision row) per manifest `Subscription.version`.

**Rationale**: Optimistic concurrency and audit lineage hang off a monotonic revision.

**Actors**: `cpt-cf-bss-subscriptions-actor-platform-operator`

#### Effective-dated composition

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-effective-dated-composition`

**`PlanLink`** `(subscriptionId, planId, effectiveFrom, effectiveTo)` governs which **plan** applies for rating/billing for interval intersection. **`AddOn`**: optional add-ons with `startDate`/`endDate`. Composition changes are effective-dated, never destructive edits of history.

**Rationale**: Interval-based composition is what lets Rating and Billing agree on "which offer applied when".

**Actors**: `cpt-cf-bss-subscriptions-actor-rating`, `cpt-cf-bss-subscriptions-actor-billing`

#### Snapshot discipline

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-snapshot-discipline`

Subscription fee artifacts MUST carry **`pricingSnapshotRef`** / catalog identifiers consistent with the manifest §4.1 contract so Billing does not re-resolve mutable catalog state for **posted** periods. (The Subscriptions-written segment of the composed ref is the `(currency, region)` binding frozen at activation — see [rating PRD](../../rating/docs/PRD.md) §1.4 `pricingSnapshotRef`.)

**Rationale**: Bill runs must reproduce from frozen inputs (AC 9), not from whatever the catalog says today.

**Actors**: `cpt-cf-bss-subscriptions-actor-billing`

#### PlanTier derivability

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-plantier-derivability`

Effective **`PlanTier`** MUST be derivable from SKU/Plan at event time; changes MUST be **effective-dated** and **Policy-gated** (manifest §4.3 invariants).

**Rationale**: Rating resolves tier-dependent pricing at `t`; a non-derivable tier breaks evaluation.

**Actors**: `cpt-cf-bss-subscriptions-actor-rating`

#### Per-sale brand attribution

- [ ] `p2` - **ID**: `cpt-cf-bss-subscriptions-fr-sale-brand-attribution`

A subscription created under a storefront brand MUST record the per-sale **`brandId`** and publish it in the pricing evaluation context, so the rating gear can match **brand-scoped** overlays ([rating PRD](../../rating/docs/PRD.md) §17.1 step 4 scope mapping). The registry `Product` declares brand *membership*; the **per-sale** `brandId` is a Subscriptions attribute.

**Rationale**: Brand-scoped commercial terms are per-sale context only this gear can supply.

**Actors**: `cpt-cf-bss-subscriptions-actor-rating`

#### Usage attribution binding

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-usage-attribution`

Usage records reach Rating from the Usage Collector keyed by tenant, usage type and resource or subject reference, with **no** `subscriptionId`, SKU or payer — the collector deliberately carries no commercial identity. This gear MUST therefore own the attribution binding: an **effective-dated** link from each provisioned resource or subject to the subscription and component (`lineKey`) that provisioned it, opened when OSS confirms provisioning (§9.2 OSS provisioning contract), closed at deprovision, and kept across ownership transfer (§6.6). It MUST expose an as-of read `(tenant, resource_ref | subject_ref, t) → (subscriptionId, lineKey)` for Rating's usage normalizer (rating SEAMS UC3(b)). The read MUST:

- answer for any `t` inside the Usage Collector replay/retention horizon and the correction window, so late and backfilled usage resolves to the component and payer it was consumed under;
- return an explicit **no match** or **ambiguous** outcome, never a best guess, so Rating can quarantine the record;
- let Rating re-key attributed usage onto the subscription's pinned ordering partition (`orderingTenantId`, §6.7) before any ordering-sensitive step.

**Rationale**: The binding is born at provisioning, which this gear drives; no other gear holds both sides of it. Without an owner no usage record can be attributed to a subscription, and usage-to-cash does not close.

**Actors**: `cpt-cf-bss-subscriptions-actor-rating`, `cpt-cf-bss-subscriptions-actor-oss-provisioning`

### 6.3 Plan Changes (Upgrade / Downgrade)

#### Change boundary and mode

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-plan-change-boundary`

`changePlan` (and add/remove add-on) updates **future** `PlanLink` rows and/or **schedules** a boundary at `effectiveFrom` (UTC). Subscriptions owns the **WHEN**: it sets `changeEffectiveAt` and `changeMode` (e.g. **immediate**, **next-cycle**, **end-of-term**) plus the up/down asymmetry policy, then emits **`SubscriptionPlanChanged`** carrying those inputs.

**Rationale**: One owner for the change boundary keeps Rating slicing and Billing artifacts on the same instant.

**Actors**: `cpt-cf-bss-subscriptions-actor-customer`, `cpt-cf-bss-subscriptions-actor-rating`

#### Proration triggers

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-proration-triggers`

| **Scenario** | **Subscription behavior** | **Billing alignment** |
|--------------|---------------------------|------------------------|
| Immediate change | Close prior plan link interval; open new `PlanLink` at `now` | Emit **delta recurring** or **one-time true-up** `BillableItem` per product policy; **never** edit posted invoice lines |
| Next cycle change | Schedule `effectiveFrom` at next anchor | First recurring charge under the new plan at the new period |
| Mid-cycle upgrade | Typically immediate; may generate **proration charge** or **credit** via Billing artifacts | Uses **credit/debit notes** if an invoice is already posted for the partial period |

Trigger summary: `changePlan` with `effectiveFrom < nextAnchor` → proration evaluation for the recurring portion; a mid-cycle **PriceWindow** (Catalog) affecting the subscribed plan → the rating layer may change the **unit rate**, subscription composition unchanged; **suspend** mid-period → credit or pause per policy; **cancel** mid-period → early-termination fee or refund per **contract**, materialized as Billing artifacts. Manifest alignment: §4.3 lists **upgrades/downgrades with proration**; §4.4 forbids mutating posted invoices — proration appears as **new billable artifacts** or **adjusting documents**.

**Rationale**: Proration is a coordination trigger here, never invoice mutation.

**Actors**: `cpt-cf-bss-subscriptions-actor-billing`

#### Proration ownership split

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-proration-ownership`

Subscriptions owns the **change boundary and mode** (`changeEffectiveAt`, `changeMode`) and emits them on `SubscriptionPlanChanged`. The **Rating gear** owns the **proration math**: it rates `planA` over `[periodStart, changeEffectiveAt)` and `planB` over `[changeEffectiveAt, periodEnd)` (half-open, UTC), prorates the recurring component on the configured **`prorationBasis`** frozen in `pricingSnapshotRef`, and applies tier-`Q` / commitment carry-vs-reset per snapshot ([rating PRD](../../rating/docs/PRD.md) §6.11); the rating pipeline slices usage at the same boundary. This PRD MUST NOT specify proration day-count math or override resolution — it only fixes the trigger, the `effectiveFrom` semantics, and the "no posted-invoice mutation" invariant.

**Rationale**: One math owner (Rating) + one boundary owner (Subscriptions) is the only split that keeps replay deterministic.

**Actors**: `cpt-cf-bss-subscriptions-actor-rating`

#### Backdated changes vs posted invoices

- [ ] `p2` - **ID**: `cpt-cf-bss-subscriptions-fr-backdated-changes`

A **commercial effective date** in the past for `PlanLink` MAY be allowed only when **contract/catalog rules** allow and **no posted invoice** would be contradicted; otherwise **split** at invoice cutoffs or issue **adjustments** (manifest §4.4). **Operational backdating** (e.g., an entitlement start in the past) MUST emit an **explicit** audit reason and may require **Policy** re-evaluation. A backdated `effectiveFrom` falling inside an already-posted invoice period MUST be rejected with a clear reason, directing the operator to the **adjustment** path (AC 6).

**Rationale**: Backdating must never contradict posted financial documents.

**Actors**: `cpt-cf-bss-subscriptions-actor-billing`, `cpt-cf-bss-subscriptions-actor-platform-operator`

#### Overlapping subscriptions

- [ ] `p2` - **ID**: `cpt-cf-bss-subscriptions-fr-overlap-cardinality`

The manifest does **not** define global cardinality. **Default (resolved):** at most **one** **`active`** subscription per **`overlapScopeKey`**, where **`overlapScopeKey`** defaults to **`(payerTenantId, catalogSubscriptionProductKey)`** — `catalogSubscriptionProductKey` is the stable Catalog key for the sellable subscription product or family, owned by the Catalog registry; **Design** binds the stored field to a published SKU/product key (dependency SEAMS SUB-G1: the registry does not expose the key yet). **Multiple concurrent `active`** subscriptions are **allowed** when they differ on **`overlapScopeKey`** (e.g. different **`resourceTenantId`**, **region**, **environment**, or **billing sub-account** when those dimensions are part of the key) or when the **Catalog** product template or **Contract** explicitly sets **`maxConcurrentActive` > 1** (or **unlimited** for wholesale/marketplace templates). If neither Catalog nor Contract sets a limit, **`maxConcurrentActive` = 1** for the default key applies. **Detection**: on **every entry into `active`** (`activate` **and** `resume`) **and on every committed change that mutates the key** (`changePlan` altering `catalogSubscriptionProductKey`; ownership **transfer** altering `payerTenantId`, §6.6), evaluate **`overlapScopeKey`** + **`maxConcurrentActive`**; reject or **queue** resolution **fail-closed** when the rule would break idempotent billing. A **cancel+new replacement** (cross-currency/region/frequency, §6.3) MAY overlap at the handover boundary only via an explicit **`supersedesSubscriptionId`** linkage — the successor's activation is exempt from the rule against exactly the subscription it supersedes, and only until that one's scheduled end. The replacement's preview/confirmation surface MUST disclose, before execution, any credit forfeiture **and the loss of grandfathered price protection** where the predecessor's pinned price is in a protected cohort — the cohort never carries across the pair (SUB-D-26, 2026-08-01).

**Occupancy read and atomic enforcement.** This gear MUST expose a batched **overlap occupancy** read: for each `(payerTenantId, overlapScopeKey)` it returns the number of `active` subscriptions holding the key (drafts excluded), the effective `maxConcurrentActive`, and the policy it came from — `catalog`, `contract`, or `platform_default` (the default of 1 above). The effective limit is always returned, so no caller has to assume one; a presence flag alone cannot evaluate a limit above one (SEAMS SUB-O5 as amended by the orders-lifecycle design). A read-time check is an early abort only: the rule MUST be enforced **atomically** where a subscription commits to `active` — re-evaluated and committed under one reservation or serialization boundary — so two concurrent activations cannot jointly exceed the limit. **Self-exemption for changes:** a `changeComposition` that adds a component to a subscription MUST NOT count that subscription against itself; the rule still applies between it and every other subscription and is evaluated inside the change's commit (§6.3 `fr-composite-change`).

**Rationale**: An explicit default cardinality with a Catalog-owned key prevents double-billing without blocking legitimate multi-instance sales.

**Actors**: `cpt-cf-bss-subscriptions-actor-catalog-registry`

#### Seat / quantity change (updateQuantity)

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-update-quantity`

**`updateQuantity`** MUST be a first-class transition carrying the §6.3 envelope (`changeEffectiveAt`, `changeMode`). The seat count the rating gear reads (pricing `quantitySource = subscription_seat_count`, D-18) MUST come only from **committed** quantity transitions — auditable provenance, never an untyped attribute edit. The committed quantity MUST be **effective-dated** (interval/revision history, same discipline as `PlanLink` §6.2) so the read model can resolve **quantity @ `t`** for any charge instant and replay — a single mutable current value cannot satisfy the rating replay contract (§9.2). Default up/down asymmetry (owned here, like plan changes): **increases MAY take effect immediately** (prorated by the rating gear at the boundary); **decreases default to `next-cycle`**, are rejected below the quantity already consumed by committed assignments (e.g. seats in use) unless policy explicitly forces revocation, and `quantity = 0` is not a quantity change — it is a `cancel` (use the cancel envelope). Quantity changes are Policy-gated where the quantity is quota-bearing, and emit **`SubscriptionQuantityChanged`** — a composition-changing event consumed like `SubscriptionPlanChanged` (AC 11 payload rules apply; naming closed by SUB-D-09 in Design slice 08). (Decisions SUB-D-02, SUB-D-09.)

**Rationale**: Mid-period seat growth is the most frequent commercial mutation on B2B subscriptions; without a transition it has no Policy gate, no proration boundary, and no provenance.

**Actors**: `cpt-cf-bss-subscriptions-actor-customer`, `cpt-cf-bss-subscriptions-actor-rating`

#### Order change intent (transactional multi-item increase)

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-composite-change`

A change order reaches this gear as **one** `changeComposition` request against one subscription ([orders-changes PRD](../../orders-changes/docs/PRD.md) `contract-chg-change-intent`): several quantity increases and component (add-on) additions, each item carrying its `orderLineId`, change kind, added quantity or component, delta price pin and optional requested effective date, plus the order reference, the process `correlationId` and an **expected revision**. This gear MUST:

1. **Commit all-or-nothing.** Apply every item in one commit or none; a failure leaves the subscription unmodified and needs no compensation (CHG-S2).
2. **Check inside the commit**, not beforehand: the expected revision equals the current `version`; the subscription is non-terminal; the resulting composition satisfies the plan's add-on rules (required, eligible, min/max/step); the overlap rule holds against every *other* subscription, with the target exempt (§6.3 Overlapping subscriptions; CHG-S2, CHG-S3). Any failing check rejects the request with a machine-readable reason.
3. **Augment, never replace.** The subscription keeps its identity; a change intent MUST NOT be expressed as cancel-and-replace (`supersedesSubscriptionId` stays reserved for the §6.3 cross-boundary replacement).
4. **Effective date.** A requested effective date becomes that item's future `changeEffectiveAt`, held as a scheduled change intent on the aggregate — the SUB-D-01/SUB-D-04 mechanism, with no new `changeMode` value (the envelope stays `immediate | next-cycle | end-of-term`); absent a date, the increase takes effect at application. This gear owns the boundary, the proration trigger and the billing consequences (CHG-S5). A requested date in the past MUST be rejected. How all-or-nothing holds for a change intent whose items carry different future dates is open (§15).
5. **Trace.** Persist the order reference per applied item and echo `orderLineId` per item on the confirmation (CHG-S1); the composition-changing events carry it (AC 11).
6. **Expose acceptance.** Durable acceptance of the request is the order's cancellation boundary (orders-changes `fr-chg-atomic-apply`), so acceptance MUST be observable through the status read (§6.1 `fr-intent-protocol`).

A **composition read** MUST return what the subscription currently carries — plan line, add-ons and quantities — together with the revision the expected-revision precondition is expressed against (CHG-S4).

This phase covers **increases** only. Decreases, removals and plan changes are not order-driven yet and keep using the direct `updateQuantity`, `removeAddOn` and `changePlan` operations; the §6.3 up/down asymmetry is unchanged.

**Rationale**: The target is live from the start, so the only compensation for a partial failure would be a decrease, which has no authored owner. A transactional commit with in-commit checks removes the need for a saga.

**Actors**: `cpt-cf-bss-subscriptions-actor-orders`, `cpt-cf-bss-subscriptions-actor-customer`

#### Committed multi-step schedules (ramps)

- [ ] `p2` - **ID**: `cpt-cf-bss-subscriptions-fr-ramp-execution`

A committed multi-step growth plan (ramp: e.g. 100 → 200 → 300 seats over quarters, or planB → planC at dates) is a **Contract term**: Contracts authors and owns the committed schedule; it materializes on the subscription as a sequence of **scheduled change intents** (`changePlan` / `updateQuantity` with future `changeEffectiveAt`), each executed here with the normal envelope, guards, and idempotency. No native schedule aggregate at launch; atomic multi-action submission (Zuora-Orders-style) is a Contracts/Design follow-up (§15). (Decision SUB-D-04.)

**Rationale**: Negotiated ramps belong with the other negotiated commitments (Contracts SoR, cf. rating T-D-14); this gear stays the executor of well-formed intents.

**Actors**: `cpt-cf-bss-subscriptions-actor-contracts`

### 6.4 Suspension and Reactivation

#### Suspend / resume semantics

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-suspend-resume`

**Suspend**: transition to `suspended`; **revoke** or **freeze** entitlements per product policy; OSS pause as confirmed by events — data-bearing resources are never deleted on suspend (§6.11 `fr-resource-disposition`). **Resume**: transition to `active`; **Policy allow** mandatory; re-issue entitlements; OSS reprovision.

**Rationale**: Suspension is a governed, reversible posture change — not a soft delete.

**Actors**: `cpt-cf-bss-subscriptions-actor-policy-engine`, `cpt-cf-bss-subscriptions-actor-oss-provisioning`

#### Suspension billing posture

- [ ] `p2` - **ID**: `cpt-cf-bss-subscriptions-fr-suspension-billing-posture`

Per the manifest risk note, **suspension vs billing alignment** MUST be explicit (e.g., **pause recurring** vs **continue to charge** for reserved capacity); the choice is **product policy** captured in subscription **attributes** and **contract** clauses — **and it travels to the gear that prices the period**: the recurring period fact carries the period's suspended interval(s) (`[suspendedAt, resumedAt)` clipped to the period) plus the `pause_recurring | continue` posture, so rating can prorate a mid-period suspension while this gear computes no money (SUB-D-07 as amended 2026-07-28; carried into this FR 2026-08-01, wave-3 review #24d).

**Rationale**: Silent assumptions about billing-during-suspension are a revenue-leak and dispute source.

**Actors**: `cpt-cf-bss-subscriptions-actor-billing`, `cpt-cf-bss-subscriptions-actor-contracts`

#### Billing-only pause (collection paused, service running)

- [ ] `p2` - **ID**: `cpt-cf-bss-subscriptions-fr-collection-pause`

The inverse of suspension MUST also be representable: a **`collectionPaused`** posture on an **`active`** subscription — service and entitlements untouched, recurring `BillableItem` emission for the paused window suppressed or deferred **per policy** (the same pause mechanism §6.5 already applies to blocked next-term recurring; Billing chooses the artifact treatment). The posture is an **auditable window** (start, end/limit, reason) bounded by Contract/Policy; it affects **collection only** — renewal **evaluation and term extension continue** per Contract, but the renewal **payment pre-check, grace entry, and dunning handoff are suspended** for renewals whose collection falls inside the pause window and run when the window ends (deferred collection; SUB-D-12) — otherwise a hardship pause would dun and suspend the very customer it protects (AC 29). Billing-cycle mechanics (pause-day limits, resume proration) remain open (§15). (Decisions SUB-D-03 + SUB-D-12; the predecessor's "Subscription Pause" scope row is honored by this shape.)

**Rationale**: Hardship/dispute pauses must not revoke service — forcing `suspended` for them does the opposite of the intent.

**Actors**: `cpt-cf-bss-subscriptions-actor-billing`, `cpt-cf-bss-subscriptions-actor-partner-admin`

### 6.5 Renewal and Grace

#### Renewal evaluation

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-renewal-evaluation`

The bound Contract is the source — the contracts gear authors `autoRenew`, the renewal term window and the notice ladder (`fr-renewal-terms`); for an uncontracted subscription the platform defaults are (`fr-platform-defaults`). **Evergreen** and **notice periods** are called out as manifest risks — notice/opt-out behavior MUST be defined in the contract templates and in the platform defaults alike. The Subscriptions **renewal job** evaluates `endDate` / term, emits the **renewal attempted** outcome; on success it extends the term; on failure it triggers the **failed renewal** path. **Every successful renewal — auto and manual alike — re-resolves the pricing-side snapshot refs eligibility-first** (SUB-D-14 as amended 2026-07-28): a non-grandfathered subscription re-binds to the current eligible row (supersessions propagate at renewal); a **grandfathered** subscription keeps its pinned generation — the refresh carries `priceEligibility` + `cohort` forward — and re-binds away **only at the first renewal after its generation's `grandfatherUntil` has passed** (pricing's `EligibilityExpirySignal`). A subscription with **`autoRenew = false`**, no pending intent, and no manual `renew` at `endDate` ends by a system-derived **end-of-term cancel** (reason `term_expired`; SUB-D-13) — it never lingers `active` unbilled.

**Rationale**: Renewal is Contract-driven; Subscriptions executes and audits it.

**Actors**: `cpt-cf-bss-subscriptions-actor-contracts`, `cpt-cf-bss-subscriptions-actor-finance-analyst`

#### Auto vs manual renewal

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-renewal-auto-manual`

**Auto**: a scheduled transition extends `PlanLink`/`endDate` and every active `AddOn` (§6.5 `fr-co-termination`) when the **payment method is valid** and the **contract allows** (Design: payment method checks). **Manual**: requires an explicit **TransitionRequest**; the same idempotency rules apply. Renewal attempts MUST be keyed to prevent **double term extension**.

**Rationale**: Idempotent renewal attempts are what make retry-driven renewal jobs safe.

**Actors**: `cpt-cf-bss-subscriptions-actor-payments`

#### Term source and evergreen semantics

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-term-source`

The term in force MUST resolve in this order: the bound Contract's term; otherwise the term on the order line that created the subscription (term duration and billing cycle are authoritative line fields — [orders-lifecycle PRD](../../orders-lifecycle/docs/PRD.md) §6.1 Line Dates); otherwise the platform default. `create` therefore accepts a term (fixed length or evergreen) and an auto-renewal flag on both the order path and the direct path. **Platform default**: evergreen with auto-renewal on. For an **evergreen** subscription the term end used by an `end_of_term` cancel and by renewal is the end of the current billing period; renewal notices (§6.5 `fr-renewal-notices`) apply to fixed-term subscriptions only. A change of term length takes effect at the next renewal.

**Rationale**: Orders sends the term as an authoritative line field and uncontracted subscriptions are the common case; without a field and a default, a quoted one-year deal has nowhere to land and an `end_of_term` cancel on an evergreen subscription has no instant.

**Actors**: `cpt-cf-bss-subscriptions-actor-orders`, `cpt-cf-bss-subscriptions-actor-contracts`

#### Co-termination

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-co-termination`

Terms MUST line up where commercial practice expects them to:

1. **Add-ons share the base term.** An add-on attached mid-term ends at the subscription's current term end and renews with it; its first interval is partial, prorated by the Rating gear. Renewal extends the `PlanLink` and every active `AddOn` together.
2. **Change-order items co-terminate with their target.** Components and quantity added by a change order end with the target subscription's current term — the premise of the Change Orders delta TCV "over the remaining term" ([orders-changes PRD](../../orders-changes/docs/PRD.md) `fr-chg-delta-total`).
3. **Co-term date for a new subscription.** `create` MAY carry a **co-term date** (on the order path it comes from the order line; carrying it there is an ask on [orders-lifecycle PRD](../../orders-lifecycle/docs/PRD.md) `fr-order-line-dates`, which today records term duration and billing cycle but no co-term date): the first term ends at that date (a shortened first term, prorated by the Rating gear), and later terms keep the normal term length aligned to it. The date comes from the bound Contract's term end, or from a payer-level co-term date where the tenant enables co-terming. **Default**: a subscription bound to a contract co-terminates with the contract term; an uncontracted one keeps its own anniversary unless the tenant enables payer-level co-terming.
4. **Boundaries.** Co-terming aligns **term** ends only. Billing period boundaries still follow the price's `billingAnchorPolicy`; a first term shorter than one billing period is billed as one prorated partial period.

**Rationale**: Resellers buy many subscriptions and add-ons mid-term for the same customer; without alignment, renewals, notices and invoices for one payer scatter across the year.

**Actors**: `cpt-cf-bss-subscriptions-actor-rating`, `cpt-cf-bss-subscriptions-actor-contracts`, `cpt-cf-bss-subscriptions-actor-orders`

#### Renewal notices and opt-out

- [ ] `p2` - **ID**: `cpt-cf-bss-subscriptions-fr-renewal-notices`

Before an auto-renewal of a **fixed-term** subscription (evergreen subscriptions: `fr-term-source`) the system MUST emit **renewal notices** at configurable intervals — platform default **30/14/7/1 days** before term end; Contract / regional template MAY override within published bounds (same override pattern as the §6.5 grace policy). Notice **triggers and intervals** are owned here; **delivery channels** are Notifications/Comms. A **renewal opt-out** MUST be processed as a scheduled non-renewal at end of term (cancel at term boundary; no further renewal attempts), idempotently.

**Rationale**: Evergreen/notice-period compliance is a manifest risk; auditable notices with an opt-out path are the mitigation.

**Actors**: `cpt-cf-bss-subscriptions-actor-contracts`, `cpt-cf-bss-subscriptions-actor-customer`

#### Failed renewal ladder

- [ ] `p2` - **ID**: `cpt-cf-bss-subscriptions-fr-failed-renewal-ladder`

| **Stage** | **Behavior** |
|-----------|--------------|
| Payment pre-check fails | Subscription remains **`active`** during **grace** (default **7 calendar days** unless the bound **Contract** specifies another value, never below the **regional template**'s statutory minimum); then **`suspended`** or **`cancelled`** per the **grace policy** and contract ladder — Design encodes timers and Payments signals |
| Post-renewal billing failure | Hand off to **dunning** (Billing/Payments §4.4–4.5); the same **grace** rules and triggers apply |
| Idempotency | Renewal attempts keyed to prevent **double term extension** |

**Grace (operational meaning).** Grace is the remediation window: the subscription **typically** stays **`active`** while retries/dunning run under the pause rules for blocked renewal recurring; exit is **time- or retry-driven** per the grace policy below, with **auditable** transitions.

**Rationale**: A defined ladder gives partners time and signal before service degradation (AC 7).

**Actors**: `cpt-cf-bss-subscriptions-actor-payments`, `cpt-cf-bss-subscriptions-actor-billing`

#### Grace policy

- [ ] `p2` - **ID**: `cpt-cf-bss-subscriptions-fr-grace-policy`

These rules make **grace** operationally testable; a bound **Contract** MAY override durations and ladder, never below the regional template's statutory minimum (contracts `fr-grace-regional`).

1. **Default duration before suspension:** **7 calendar days** from **grace start** (first auditable **failed renewal pre-check** or **aligned post-renewal billing failure** for the renewal attempt). **Jurisdiction:** where a regional template sets a statutory minimum, the effective duration MUST come from the bound **Contract** (floor-checked by the contracts gear) or, for an uncontracted subscription, from the platform defaults of that territory; otherwise **7 days** is the platform default.
2. **Recurring during grace:** **`BillableItem(kind=recurring)` for the renewal term that is blocked by the failure MUST NOT be emitted while grace runs**; on renewal **success** (including a §6.5(5) late success and the SUB-D-13 post-suspension revival) it is emitted with its original key; on **grace resolving to failure it is never emitted** — a fact must never exist for a term that never started (wording aligned 2026-08-01 to the 2026-07-28 billing-pass fix, wave-3 review #19a: the earlier "until … or grace resolves to failure" literally licensed emitting on failure, and the PRD is the requirement source). **Usage-rated** charges **MAY continue** until **`suspended`** unless **Contract** or **Policy** explicitly freezes usage for the grace window.
3. **Per-contract configurability and SoR:** **configurable per contract** (or per contract template). **Authoritative** commercial terms (**grace length**, ladder, billing posture) live on the bound **Contract** (contracts gear `fr-grace-regional`) and, for an uncontracted subscription, on the platform defaults (`fr-platform-defaults`). **Subscription** MUST store **evaluated fields** (§1.4) at renewal evaluation time for **audit**, **idempotent** renewal jobs, and replay.
4. **Grace → `suspended` (or `cancelled`) trigger:** **hybrid — whichever is first:** **(i)** the current grace interval **elapses** without successful renewal, **or** **(ii)** **Payments** declares **no further automated retries** for the failure. Move to **`cancelled`** per **contract-defined** steps after suspend or final dunning — with the platform-default terminal step where the bound Contract defines none and for every uncontracted subscription (SUB-D-16): a nonpayment `suspended` subscription dwells at most **90 days** (tenant-configurable; a Contract ladder overrides), then a system `cancel` (reason `nonpayment_exhausted`) fires; the effective dwell is **resolved and stored at suspension time** as an evaluated field (rule 3 discipline) — later configuration or Contract changes govern future suspensions only, never an in-flight deadline; the post-suspension revival path (grace policy 5a / Design §4.3b) is bounded by the dwell.
5. **Late success inside grace (term continuity):** when renewal succeeds during grace, the new term MUST start at the **old term end** (backdated — continuous coverage, no gap), and the previously blocked next-term recurring is emitted with its **original** `(subscriptionId, billing period, lineKey)` key. **Resume after a grace-driven suspension** requires the blocking payment failure to be **resolved** (successful renewal/payment, or an audited operator override) — `resume` alone MUST NOT restore unpaid service (§6.1 guard table).

**Rationale**: Grace defaults answered in the PRD (not left to Design) are what make the ladder product-testable.

**Actors**: `cpt-cf-bss-subscriptions-actor-contracts`, `cpt-cf-bss-subscriptions-actor-payments`

#### Platform defaults and contract binding

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-platform-defaults`

A Contract is optional ([orders-lifecycle PRD](../../orders-lifecycle/docs/PRD.md) `fr-order-create`; [contracts PRD](../../contracts/docs/PRD.md) `fr-renewal-terms`). The aggregate MUST carry an optional **contract binding** (`contractId` and contract version), and:

1. **Uncontracted is first-class.** A subscription with no binding is governed by the **platform defaults** (§1.4): auto-renewal, term (fixed length or evergreen), billing anchor instant (the activation instant, `serviceActivatedAt`; the bound price's `billingAnchorPolicy` then places the boundaries), notice ladder (30/14/7/1 days), grace (7 days), nonpayment dwell (90 days), acceptance requirement (none), concurrent-active cardinality (1), data retention window (30 days, §6.11), archive dwell (90 days, §6.11). The set MUST be complete — every rule in this PRD that names the Contract as the source of a term resolves, for an uncontracted subscription, from this set — and is published per tenant within platform bounds. Term and auto-renewal resolve per `fr-term-source` (default: evergreen, auto-renewal on).
2. **Explicit switch.** A bound Contract's terms supersede the defaults for that subscription only through a recorded binding with an effective instant; a contract existing for the same payer MUST NOT change a subscription's terms implicitly (contracts `fr-renewal-terms`). Whether a new binding changes the in-flight term or only the next one is open (§15).
3. **Evaluated fields with provenance.** Whatever the source, the terms in force are stored as evaluated fields at evaluation time together with their provenance (`contract` + version, or `platform_default`), so replay and audit do not depend on today's defaults (grace policy rule 3).
4. **Contract end.** `ContractTerminated` and the contract-expired event for a bound contract MUST be consumed and recorded on the aggregate; the resulting subscription behaviour — end-of-term cancel, or reversion to the platform defaults — is open (§15) and MUST be decided before the contracts gear publishes those events.

**Rationale**: Contracts is a first-draft gear and self-service orders typically carry no contract, so the defaults are what most subscriptions will run on. Specified as a state, they cannot silently harden into de-facto terms that later change under live customers.

**Actors**: `cpt-cf-bss-subscriptions-actor-contracts`, `cpt-cf-bss-subscriptions-actor-platform-operator`

### 6.6 Multi-Tenant Ownership

#### Tenant axes

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-tenant-axes`

| **Axis** | **Use** |
|----------|---------|
| `resourceTenantId` | Operational owner of resources tied to the subscription |
| `payerTenantId` | Financial responsibility (consolidated billing) |
| `sellerTenantId` | Channel/marketplace seller when applicable |

For an order-originated subscription the axes are taken from the order, fixed at order submit. The party that placed the order is recorded as the **initiating actor** — an authorization and audit attribute, not a fourth axis.

**Rationale**: The three axes are the multi-tenant backbone every downstream consumer keys on.

**Actors**: `cpt-cf-bss-subscriptions-actor-ams`

#### Delegation proofs

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-delegation-proofs`

Cross-tenant admin actions MUST carry an **auditable delegation proof** (manifest §2.1.3); an action without a valid delegation MUST be rejected, and the audit record MUST include the explicit proof reference (AC 10).

**Rationale**: Reseller hierarchies make undelegated cross-tenant mutation a critical-severity risk.

**Actors**: `cpt-cf-bss-subscriptions-actor-partner-admin`

#### Hierarchy by reference

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-hierarchy-reference`

Commercial roll-ups follow **account** and **OrgTier** context from AMS; Subscriptions MUST NOT invent tenant topology — it **references** the AMS/BSS account binding only.

**Rationale**: One identity SoR; BSS projections never fork it.

**Actors**: `cpt-cf-bss-subscriptions-actor-ams`

### 6.7 Event Model

#### Producer inventory

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-event-producers`

Subscriptions **MUST emit** (CloudEvents 1.0, tenant-scoped, minimal PII): `SubscriptionCreated`, `SubscriptionActivated`, `SubscriptionSuspended`, `SubscriptionResumed`, `SubscriptionCancelled`, **`SubscriptionPlanChanged`**, `BillableItemCreated` (`kind ∈ {recurring, one_time}` — the one-time lane per SUB-D-24, 2026-08-01), `EntitlementPostureChanged` (the manifest's `EntitlementIssued` / `EntitlementRevoked` are produced by License Manager on materialization — alignment tracked in §15), `OwnershipTransferRequested`, `OwnershipTransferApproved`, `OwnershipTransferCompleted` (transfer per manifest §4.11). This is the manifest **baseline**; the **secondary auditable events** this PRD mandates elsewhere — intent scheduling/un-scheduling (AC 22), renewal outcome and grace entry/exit (AC 7), notice triggers (AC 19), the collection-pause window (AC 24), the quantity composition event (AC 23), the trial conversion/extension events (AC 16–17), acceptance confirmation (AC 25), quota warning/exhaustion/restore (AC 14), and the **seat bind/release pair `SeatBound`/`SeatReleased`** (§6.9 seat bindings; added 2026-08-01, wave-3 review #24g) — extend this inventory with naming closed normatively in **Design slice 08** (SUB-D-09).

**Rationale**: The manifest §4.3 producer inventory is the downstream integration surface.

**Actors**: `cpt-cf-bss-subscriptions-actor-billing`, `cpt-cf-bss-subscriptions-actor-rating`, `cpt-cf-bss-subscriptions-actor-analytics`

#### Payload completeness (business rules, not schema)

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-event-payload-completeness`

This PRD MUST NOT enumerate **event attribute/extension names** or wire formats; those belong in **Design** (`DESIGN-subscriptions-*`). Normative intent only: every **lifecycle** producer event MUST carry **enough identity, tenancy, correlation, and time** context that consumers can **route**, **deduplicate**, and **replay** in stream order **without** a mandatory undocumented side channel for those concerns. Events that **change commercial composition** (including plan changes) MUST carry **enough snapshot-oriented commercial context** that **Rating** and **Billing** can stay **aligned** on the same effective offer for the affected period and process commands **idempotently** (AC 11).

**Rationale**: Sufficiency, not schema, is the PRD-level contract; Design owns the field matrix.

**Actors**: `cpt-cf-bss-subscriptions-actor-rating`, `cpt-cf-bss-subscriptions-actor-billing`

#### Consumers

- [ ] `p2` - **ID**: `cpt-cf-bss-subscriptions-fr-event-consumers`

**OSS Provisioning** acts on subscription/entitlement changes; **Policy Engine** receives post-change confirmations as required by integration Design; **Billing** ingests recurring items and aligns periods/proration; **Orders Workflow** consumes the per-intent confirmations and failures of the requests it submitted (§6.1 `fr-intent-protocol`); **Analytics/DWH** consumes facts.

**Rationale**: The manifest §4.3 consumer list bounds who may depend on these streams.

**Actors**: `cpt-cf-bss-subscriptions-actor-oss-provisioning`, `cpt-cf-bss-subscriptions-actor-analytics`

#### Ordering

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-event-ordering`

Order **MUST** be preserved **within `(tenantId, aggregateId)`** with **`aggregateId = subscriptionId`** (the manifest §4.2 note on rating aggregation applies analogously to subscription command handling). Per that note and the [rating PRD](../../rating/docs/PRD.md), `tenantId` in this ordering key denotes the **`resourceTenantId`** (operational owner) so that subscription command ordering and downstream Rating partition ordering share the same key. The ordering tenant is **pinned at creation** as an immutable **`orderingTenantId`** (= `resourceTenantId` at creation): an ownership **transfer** (§6.6) rebinds the commercial tenant axes but **never** the ordering/partition key — pre- and post-transfer events stay on one partition, and `OwnershipTransferCompleted` carries both old and new axes on that partition so consumers re-key their own projections (SUB-D-06, AC 26).

**Rationale**: A shared ordering key is what lets Rating consume composition changes without reorder hazards (AC 3).

**Actors**: `cpt-cf-bss-subscriptions-actor-rating`

### 6.8 Billing Alignment

#### Recurring idempotency

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-recurring-idempotency`

**`BillableItem(kind=recurring)`** MUST be idempotent on **`(subscriptionId, billing period, lineKey)`** (manifest §4.3; the `lineKey` component dimension added by SUB-D-19, 2026-07-28 — a subscription is a plan line plus N add-on lines, each with its own catalog keys, so a subscription-wide key could represent only one of them): at most one recurring item per key even under bill-run retries (AC 5). **Ownership split (SUB-D-07):** what this gear emits is the **money-free recurring period fact** — period identity from the billing anchor, the §6.8 traceability tuple, `pricingSnapshotRef`, the pause/intent posture, the **suspended interval(s) + suspension-billing posture**, and the **`payerTenantId` in force at the period start** (the SUB-D-07 amendment fields, carried into this FR 2026-08-01 — wave-3 review #24d; the payer axis re-anchored from cut time by SUB-D-20 so a late/revival cut still lands the period on the payer it was consumed under); the **rating gear prices** the recurring component from the frozen snapshot (flat / per-unit × committed quantity / the hybrid recurring line — rating PRD §6) and the **priced line inherits the fact's idempotency key**; Billing posts. This gear owns the period **cut** (anchor, pauses, pending intents, the key) and never the amount — the same WHEN/MATH split as plan changes (AC 27). Every cut resolves **as of the period it describes** — components from the effective-dated composition over the period, never the live composition at run time (SUB-D-20/21).

**Rationale**: Recurring double-charges are the classic bill-run failure; the key kills the class.

**Actors**: `cpt-cf-bss-subscriptions-actor-billing`

#### No retro-edit

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-no-retro-edit`

Posted invoice lines MUST NOT be rewritten; subscription corrections emit **new** billable or **adjustment** paths (manifest §4.4).

**Rationale**: Posted-document immutability is the financial-audit bedrock shared with Billing and Rating.

**Actors**: `cpt-cf-bss-subscriptions-actor-billing`

#### Traceability

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-billing-traceability`

Items MUST trace **per component**: each item carries **`subscriptionId`**, its **`lineKey`**, and **its own component's** **`skuId`/`planId`/`priceId`** + **`pricingSnapshotRef`** (manifest §4.4 itemization; SUB-D-19 — re-scoped 2026-08-01, wave-3 review #7: the singular tuple would stamp the plan's catalog keys on add-on lines). Items of an **order-originated** component additionally carry the **order reference** of the line that sold it — `orderId`, `orderLineId`, and the order's external reference where present — because billing documents derive from the subscription, not from order events; without it a purchase-order number shown on the order never reaches the invoice.

**Rationale**: Charge-to-catalog lineage is what partners and auditors reconcile against.

**Actors**: `cpt-cf-bss-subscriptions-actor-finance-analyst`

#### Dataset separation

- [ ] `p2` - **ID**: `cpt-cf-bss-subscriptions-fr-dataset-separation`

Subscription state ≠ invoice posted state; late usage adjustments remain a Rating→Billing concern (manifest §4.2). This section states **Billing invariants** for artifacts Subscriptions coordinates with Billing; it does **not** describe client control-plane operations (§9.1) — REST paths, HTTP methods, error codes, and header field names belong in **Design**.

**Rationale**: Keeping lifecycle state and posted financial state as separate datasets prevents accidental coupling.

**Actors**: `cpt-cf-bss-subscriptions-actor-billing`

### 6.9 Entitlement Posture and Issuance

> **Note (re-split 2026-10-02):** until this revision the section carried four requirements — issue/revoke on transitions, assignment from the plan's grant set, a point-of-use check contract at p95 < 100 ms, and quota tracking with soft/hard limits. Two of them belong to platform gears that now exist in this repository: the check is answered by `license-resolver` from grants held in `license-enforcement`, and counting with soft/hard limits is `quota-enforcement`'s. Materializing the posture — issuing, renewing, replacing, suspending and revoking the licenses, packs and limits that back a subscription — is the **License Manager** gear's (upstream `PRD-license-manager-202610021000`), which also serves installed products outside the platform through the same issuance path. What stays here is the commercial decision: what a subscription confers at any instant, and the idempotent request that makes License Manager materialize it. `fr-entitlement-check-contract` and `fr-entitlement-quota-limits` are withdrawn; `nfr-entitlement-check-latency`, AC 13 and AC 14 are replaced (§7.1, §12).

#### Posture change and issuance request on subscription transitions

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-entitlement-issue-revoke`

On a successful **resource-affecting** transition whose outcome changes what the subscription confers (activate, suspend, resume, cancel, Policy-gated plan/add-on changes, phase boundaries, renewal), Subscriptions MUST compute the subscription's new **entitlement posture** — the resolved grant set per `fr-entitlement-assignment`, quantity, validity, holder/payer/seller tenants, active phase and order reference — and MUST submit it to License Manager as an issuance request (`issue`, `renew`, `replace`, `suspend`, `reinstate`, `revoke` or `convert`) carrying a **commercial event token** derived from `(subscriptionId, revision, transition)`. The request MUST be idempotent on that token, MUST be retried with the same token until acknowledged, and MUST NOT be resubmitted under a different token for the same committed transition. Subscriptions MUST emit its own auditable posture-change event on commit (`EntitlementPostureChanged`, §6.7); the materialization events are License Manager's (AC 12). A request License Manager refuses for an eligibility or bounds reason MUST be surfaced on the transition request as an exception and MUST NOT roll back the committed transition silently; the handling path (hold, operator queue) is Design.

**Rationale**: Posture must be a deterministic function of committed state, and exactly one party must turn it into licenses and grants: one journal of "what was issued for which commercial event" lives in License Manager, while the decision stays with the commercial aggregate.

**Actors**: `cpt-cf-bss-subscriptions-actor-license-manager`, `cpt-cf-bss-subscriptions-actor-policy-engine`

#### Assignment from plan definitions

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-entitlement-assignment`

On activation, phase boundary, and committed plan/add-on change, entitlements (feature flags, usage quotas, resource quotas) MUST be assigned from the **plan's published grant set** — including the per-phase map where the plan is phased (pricing gear, `phase→grant-set` — D-41) — with immediate or end-of-cycle effective dates aligned to the transition's `changeMode`. The catalog authors the templates; this gear resolves the assignment per subscription and hands it to License Manager in the issuance request (`fr-entitlement-issue-revoke`); it holds no grant store, no check surface and no counter.

**Rationale**: One authoring home (catalog grant sets), one assignment home (here) and one materialization home (License Manager) keep entitlements reproducible per revision.

**Actors**: `cpt-cf-bss-subscriptions-actor-pricing`, `cpt-cf-bss-subscriptions-actor-license-manager`

#### License state and findings on the subscription

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-entitlement-license-state`

Subscriptions MUST consume, per subscription line, the state License Manager reports for the licenses and packs that back it (issued, bound, active, graced, suspended, expired, terminated, replaced; pack pending) and the findings that matter commercially (usage gap, concurrent use, issuance pending beyond tolerance), MUST expose them on the subscription read model, and MUST treat an issuance pending beyond the §7.1 hand-off budget as an auditable exception — never as an activated entitlement. Renewal, grace and cancellation remain this gear's decisions; a license state reported by License Manager never drives a subscription transition on its own.

**Rationale**: Operators and partners read the subscription first; the licensing facts behind it must be visible there without making License Manager a second lifecycle authority.

**Actors**: `cpt-cf-bss-subscriptions-actor-license-manager`, `cpt-cf-bss-subscriptions-actor-partner-admin`, `cpt-cf-bss-subscriptions-actor-platform-operator`

### 6.10 Trial Runtime and Conversion

#### Trial provisioning

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-trial-provisioning`

A trial subscription MUST be created from a **Catalog-defined trial offer** (trial plan/SKU or a leading trial **phase**) with configurable duration, per §6.1 trial rules (no `trial` status; evaluated attributes + `PlanLink`/snapshot pointers persisted). Feature access during trial follows the **trial-phase grant set** (§6.9 assignment).

**Rationale**: Catalog-first trials keep trial economics reproducible; the phase machinery already carries duration and grants.

**Actors**: `cpt-cf-bss-subscriptions-actor-pricing`

#### End-of-trial conversion

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-trial-conversion`

At trial end the system MUST convert per the plan's phase schedule (`convertsToPhaseId`): advance the phase boundary, authorize payment where required (Payments, per Design — without re-entering payment details where a method is on file), submit a `replace` issuance request to License Manager for the target phase's grant set with binding carry-over and **continuity** (no access gap, no re-activation of an installed product), and emit the composition-changing event. Where the target phase is paid, the boundary MUST NOT advance without a **valid payment method on file**: an unconverted trial with no method follows the expiry path (`fr-trial-expiry`) and no paid-phase entitlement is issued. A payment **failure** with a valid method on file follows the §6.5 grace ladder. Conversion processing MUST be idempotent (**zero missed / zero double conversions**).

**Rationale**: Conversion is the revenue moment of a trial; it must be deterministic, continuous, and ladder-protected.

**Actors**: `cpt-cf-bss-subscriptions-actor-payments`, `cpt-cf-bss-subscriptions-actor-rating`

#### Early conversion (convertTrial)

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-trial-early-conversion`

**`convertTrial`** MUST be a first-class operation ("skip the trial, start paying now"): an explicit `TransitionRequest` that advances the phase boundary to `now` (the phase-axis twin of `changePlan` — the boundary instant is consumed by the rating gear like any `changeEffectiveAt`), submits the `replace` issuance request for the target phase, and emits a first-class conversion event — Policy-gated where resource-affecting, idempotent on `(subscriptionId, idempotencyKey)`. This extends the manifest §4.3 `TransitionRequest.type` list — manifest alignment tracked in §15.

**Rationale**: Modeling early conversion as an untyped attribute edit loses the Policy gate, entitlement re-issue, eventing, and idempotency.

**Actors**: `cpt-cf-bss-subscriptions-actor-customer`, `cpt-cf-bss-subscriptions-actor-partner-admin`

#### Trial expiry without conversion

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-trial-expiry`

An unconverted trial at expiry MUST follow the configured end action using normal transitions (typically **cancel**; never a bespoke terminal status — §6.1): a `revoke` issuance request submitted to License Manager, auditable events emitted, and an optional **win-back hook** event published (campaign content and delivery = Notifications/Comms, out of scope §5.2).

**Rationale**: Expiry must be as governed and auditable as any other lifecycle exit.

**Actors**: `cpt-cf-bss-subscriptions-actor-oss-provisioning`

#### Trial extension

- [ ] `p2` - **ID**: `cpt-cf-bss-subscriptions-fr-trial-extension`

A trial extension MUST be an **approval-gated** operation (Approval record per §6.1 high-risk pattern) that moves the conversion date and the trial-phase end consistently, with audit; the approval policy (automatic / manual / threshold-based) is a Product decision (§15).

**Rationale**: Extensions change revenue timing; they need the same governance as other high-risk transitions.

**Actors**: `cpt-cf-bss-subscriptions-actor-partner-admin`

### 6.11 Data Retention and Offboarding

#### Resource and data disposition on suspend and cancel

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-resource-disposition`

In OSS, deprovisioning a resource deletes it. This gear MUST therefore drive resource legs so that customer data is never lost by a lifecycle transition alone:

1. **Suspend pauses, never deletes.** A `suspend` pauses data-bearing resources (VM disks, volumes, object buckets, backups) and blocks access; it MUST NOT delete them, for the whole suspension including the nonpayment dwell (§6.5 grace policy rule 4). Non-data resources that cannot be held paused (for example a floating IP) MAY be released, with audit. `resume` restores access to the same resources.
2. **Cancel opens a retention window.** At the cancel's effective instant entitlements are revoked and service stops; data-bearing resources enter a **retention hold** for the **data retention window** — platform default **30 days**, tenant-configurable within platform bounds; a bound Contract or regional template MAY extend it, never shorten it below a statutory minimum. During the window the customer can **export** the data read-only, and no recurring charge accrues unless a Contract defines a retention fee. At the window's end OSS deletes the resources; OSS confirms the deletion by event and the purge is audited.
3. **Same rule for every cancel reason.** `customer`, `operator`, `term_expired`, `nonpayment_exhausted`, trial expiry and `order_compensation` all open the window; only the draft void opens none, since nothing was provisioned.
4. **Warnings.** Pre-deletion notices fire 7 days and 1 day before the window ends (delivery via Notifications), and a notice fires when the window opens.
5. **Holds.** A legal hold on the subscription or its payer (`fr-record-retention`) suspends the deletion timer until released.
6. **Timeline here, execution in OSS.** This gear owns the timeline — window start and end stored as evaluated fields with provenance, like grace — and emits auditable events for window start, each notice and purge completion (naming in Design slice 08). OSS executes pause, export and deletion (SEAMS SUB-E4).

The usage attribution binding stays resolvable through the window, so late usage before the cancel instant still attributes, and closes at purge. This follows the manifest's offboarding order: entitlements termination, then final rating and billing closeout, then the retention and legal-hold decision.

**Rationale**: For IaaS a deprovisioning suspend or an immediate deprovisioning cancel is customer data loss — the riskiest outcome a subscription transition can have.

**Actors**: `cpt-cf-bss-subscriptions-actor-oss-provisioning`, `cpt-cf-bss-subscriptions-actor-customer`, `cpt-cf-bss-subscriptions-actor-platform-operator`

#### Subscription record retention, legal hold and personal data

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-fr-record-retention`

1. **Archive.** The retention job moves a `cancelled` subscription to `archived` after the **archive dwell** — platform default **90 days** after the cancel's effective instant and never before its data retention window ends; tenant-configurable.
2. **Keep.** Archived records — the aggregate, its revisions, transition requests, evaluated fields and audit — are inputs to invoice reproduction (AC 9) and MUST be kept for at least the legal or contractual retention period for financial records (manifest §9: typically at least 7 years), configurable upward per tenant or jurisdiction; after it the retention job purges them, with audit.
3. **Legal hold.** A legal hold on a subscription or its payer blocks both the purge of its records and the deletion of its retained resources until released; placing and releasing a hold are audited.
4. **Personal data.** Records reference parties by stable tenant and account identifiers, not raw personal data; an erasure request tombstones any personal-data reference without rewriting history used to reproduce posted documents — the same mechanism as the ledger's right-to-erasure.

**Rationale**: Archived subscriptions are part of the financial evidence chain, so their retention follows the financial-record rule rather than an open-ended "kept forever".

**Actors**: `cpt-cf-bss-subscriptions-actor-platform-operator`, `cpt-cf-bss-subscriptions-actor-finance-analyst`

## 7. Non-Functional Requirements

### 7.1 NFR Inclusions

> Baselines carried forward from `PRD-subscriptions-entitlements-202601120119` (PRD-0001 SLAs); they MUST be reconciled with the program NFR workshop — the workshop overrides on conflict (§15).

#### Lifecycle control-plane latency

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-nfr-lifecycle-latency`

Synchronous commit class for `activate`, `suspend`, `resume`, `changePlan`, `cancel`: **p95 < 1s** — the bound covers the **synchronous intent commit** of the request (validate + idempotency + guard + Policy pre-check + versioned write). For OSS-blocking edges (activation/provisioning legs) the status change itself is asynchronous (`pending → approved → applied`) and is **not** inside this bound — the provisioning confirmation is OSS-paced; one number, one operation, so a load test and product sign-off measure the same thing (Design slice 01 NFR allocation is the authority).

#### Entitlement posture hand-off

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-nfr-entitlement-handoff`

Issuance request submitted to License Manager within **5 s** of the committing transition at p95, acknowledged or retried with the same token; a request pending beyond **60 s** is an auditable exception (§6.9 `fr-entitlement-license-state`). The predecessor's **p95 < 100ms** check figure moved with the check to the platform `license-resolver` (re-split 2026-10-02).

#### Recurring generation cut

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-nfr-recurring-cut`

Recurring `BillableItem` generation (schedule/cut): **daily by 00:00** per the productized window in Design.

#### Proration monetary accuracy

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-nfr-proration-accuracy`

Proration / plan-change monetary alignment: **100% accuracy** vs policy (the math itself is the Rating gear's; this NFR binds the end-to-end alignment).

#### Horizontal partitioning and scale

- [ ] `p2` - **ID**: `cpt-cf-bss-subscriptions-nfr-horizontal-partitioning`

Horizontal partitioning by tenant for subscription reads/writes; support **100K+ active subscriptions per tenant**; bulk read models for account roll-ups; avoid N+1 Policy calls via batch where contractually safe.

#### Operational latency baselines

- [ ] `p2` - **ID**: `cpt-cf-bss-subscriptions-nfr-operational-baselines`

Carried from the predecessor's module specifications, pending the same NFR-workshop reconciliation: state transition **p95 < 500ms** (the predecessor's figure; where it conflicts with `nfr-lifecycle-latency`, the 1 s synchronous-commit bound governs until the workshop sets one number); subscription query **p95 < 200ms**; entitlement posture hand-off to License Manager **< 5s** (`nfr-entitlement-handoff`); event delivery to consumers **p95 < 30s**; recurring charge accuracy **100%** with **zero duplicates** (§6.8).

#### Intent seam budget for orchestrating callers

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-nfr-intent-seam-budget`

For the intents of §6.1 `fr-intent-protocol`, this gear MUST publish a p95 and p99 for **accepting** an intent and for **confirming** it (excluding the OSS-paced provisioning leg, as in `nfr-lifecycle-latency`), the sustained submission rate a caller may offer, and the back-pressure signal returned above that rate (SEAMS SUB-O15). Values are set at the NFR workshop; until then callers size their step deadlines against the §7.1 baselines.

### 7.2 NFR Exclusions

- **PSP/dunning retry timing internals** — Payments/Billing (§4.5); this PRD consumes signals only.
- **Tax computation and statutory invoicing performance** — Billing/Tax.
- **Revenue recognition throughput** — Finance/Billing.

## 8. Five Quality Vectors Analysis

| **Quality Vector** | **Show-Stopper Requirements** | **Rationale** |
|--------------------|-------------------------------|---------------|
| **🚀 Efficiency** | Bulk read models for account rollups; avoid N+1 Policy calls via batch where contractually safe; **95% reduction in manual subscription operations** — zero manual intervention for standard lifecycle transitions. | Subscription lists power portals and support at scale; manual ops do not scale past ~1000 customers. |
| **🔒 Reliability** | State machine + idempotency + ordering invariants; **zero missed recurring charges**; DLQ/replay for failed transitions; daily reconciliation checks (§17.1). | Revenue and entitlement mistakes are existential risk; missed charges are silent leakage. |
| **⚡ Performance** | The §7.1 baselines: lifecycle control-plane **p95 < 1s**; entitlement posture hand-off to License Manager **< 5 s**; recurring generation **daily by 00:00**; proration alignment **100% accuracy**; horizontal partitioning by tenant. Baseline from the predecessor PRD (PRD-0001 SLAs) — the program NFR workshop overrides if in conflict. | The same SLAs block onboarding and billing accuracy at scale; explicit targets make the vector testable. |
| **🛡 Security** | Strict tenant isolation; delegation proofs for cross-tenant ops; audit on every transition with SOX-grade correlation IDs; encryption at rest and in transit. | Commercial data is sensitive; cross-tenant leakage is critical severity. |
| **🔄 Versatility** | Support multiple commercial models (usage, recurring, hybrid, fixed-term, evergreen, prepaid) via `PlanLink`/add-ons and Contract terms without breaking aggregate ordering; extensible entitlement posture (flags, quotas, limits — materialized by License Manager). | Channel SKUs and enterprise deals vary widely; product evolution requires extensible entitlements. |

## 9. Public Library Interfaces

### 9.1 Public API Surface

#### Control-plane operations contract

- [ ] `p2` - **ID**: `cpt-cf-bss-subscriptions-interface-control-plane`

**Shapes in Design**; here: **business operations** (what clients must be able to do), not REST mapping. How each operation is exposed (paths, methods, idempotency/concurrency header bindings, OpenAPI) is specified in **Design** alongside error contracts. Resources (names per manifest): `Subscription`, `Entitlement`, `AddOn`, `PlanLink`, `TransitionRequest`, `Approval`.

| **Operation** | **Verb(s)** | **Idempotency / concurrency (requirements)** |
|---------------|-------------|-----------------------------------------------|
| create | `create` | Caller-supplied **idempotency key**, deduped on `(orderingTenantId, operation, key)` (§4.1); optional **order reference** and the line's add-on selection (§6.1 `fr-order-originated-create`); optional term, auto-renewal flag and **co-term date** (§6.5) |
| import (system / operator) | `import` | idempotent on `(source system, legacy identifier)`; batch with per-item outcome; adopts listed resources, no provisioning, no one-time fact (§6.1 `fr-import`) |
| get / list | `get`, `list` | — |
| activate / suspend / resume / cancel | `activate`, `suspend`, `resume`, `cancel` | **Idempotency key** + **optimistic concurrency** on subscription **version** (Design maps to concrete headers); the start is the `activate` commit instant, never order- or caller-supplied (the import mode is the only exception); `cancel` carries `cancelMode` and a reason from the closed set (§6.1 `fr-cancel-reasons`); `suspend` MAY carry `resumeAt` (§6.1) |
| plan change | `changePlan` | same |
| add-on change | `addAddOn`, `removeAddOn` | same |
| quantity change | `updateQuantity` | same; §6.3 envelope (SUB-D-02) |
| order change intent | `changeComposition` | idempotency key + **expected revision** as precondition; all-or-nothing; per-item `orderLineId` echoed (§6.3 `fr-composite-change`) |
| composition read | `getComposition` | read-only; current plan line, add-ons and quantities plus the revision a change is expressed against (§6.3) |
| overlap occupancy read | `getOverlapOccupancy` | read-only, batched; per `(payerTenantId, overlapScopeKey)`: active count, effective `maxConcurrentActive`, provenance (§6.3) |
| request status read | `getTransitionRequest` | read-only; by request identifier, or by `orderId` + `orderVersion` + `orderLineId` + wave (§6.1 `fr-intent-protocol`) |
| withdraw accepted request | `withdrawTransitionRequest` | idempotency key; only before the request applies; an outstanding OSS work order is cancelled with a compensating deprovision first (slice 01 §3.6); ends it as `failed` with reason `withdrawn` (§6.1) |
| usage attribution read | `resolveUsageAttribution` | read-only, batched, as-of `t`; explicit no-match / ambiguous outcomes (§6.2 `fr-usage-attribution`) |
| trial conversion / extension | `convertTrial`, `extendTrial` | same; `extendTrial` requires **Approval** (§6.10) |
| manual renewal | `renew` | same; keyed against double term extension (§6.5) |
| un-schedule pending intent | `unschedule` | same; references the pending intent it voids (§6.1, AC 22) |
| collection pause window | `pauseCollection`, `resumeCollection` | same; auditable window attributes (§6.4) |
| acceptance confirmation | `confirmAcceptance` | same; stamps `customerAcceptedAt` (§6.1) |
| ownership transfer | `transfer` | same; requires **Approval** + delegation proof (§6.6) |
| archive (retention) | `archive` | same; the `cancelled → archived` edge, normally submitted by the retention job (system actor), operator-submittable with audit (§6.1) |
| entitlement posture re-drive (internal / admin) | `reissueEntitlements` | **Audit** mandatory; re-submits the committed posture to License Manager under the original commercial event token (idempotent, §6.9); shapes in Design |
| in-flight-subscriber presence read (pricing-facing) | `getInFlightPresence` | read-only; pricing submits a price-id set, receives the count of non-terminal subscriptions whose pinned `pricingSnapshotRef` references any of them (SEAMS SUB-P8; added 2026-08-01, wave-3 review #18) |
| seat binding (consuming-system write pair) | `bindSeat`, `releaseSeat` | **Idempotency key** + **audit**; fail-closed above the committed quantity (the §6.3 decrease guard's counter); rejected while `suspended` (frozen posture, §6.4); registered 2026-07-28 — shapes in Design (slice 05) |

### 9.2 External Integration Contracts

#### Billing handoff contract

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-contract-billing-handoff`

**Protocol/Format**: `BillableItemCreated(kind=recurring)` — the **money-free recurring period fact**, cut **per billable component interval** and idempotent per `(subscriptionId, billing period, lineKey)` (SUB-D-19; `lineKey` = `plan#n` / `addon:{addOnId}#n` per SUB-D-21 — the same coordinate rating carries in its period-driven unit key, so the inheritance rule below holds for plan + add-on lines alike), carrying that component's `{subscriptionId, skuId, planId, priceId}` + `pricingSnapshotRef` + the pause/intent posture + the **suspended interval(s) with the suspension-billing posture** + the **period-start `payerTenantId`** (SUB-D-07 amendment fields, added to this contract 2026-08-01 — wave-3 review #24d; payer axis per SUB-D-20), **no monetary column**; the rating gear prices the recurring component and the priced line inherits the fact's key before Billing posts (SUB-D-07, §6.8); proration materialized only as new billable or adjusting artifacts; posted invoices immutable (§6.8). `SubscriptionCancelled` carries the current **term window + containing billing-period identity** as Billing's ETF/credit join key; the derivation is restricted to the `customer`/`operator` reason class (SUB-D-25).

**One-time charges (T-D-18 adoption, 2026-07-28 — formalised as SUB-D-24, 2026-08-01; CONFIRMED in the 2026-08-01 veto round; **re-based on phase-entry occurrences 2026-09-20, pricing D-375 — a contract-version break**)**: `chargeKind = one_time` rows (`one_time_setup` is withdrawn from the pricing contract; there are exactly three kinds) are **never rated** — the rating pipeline synthesizes no evaluation unit for them (rating T-D-18). This gear MUST emit `BillableItemCreated(kind=one_time)` **at its qualifying instant** — subscription activation, or **trial conversion** for a trialed plan (the first non-trial phase entry) — as an **amount-less fact** carrying the qualifying instant, the component's traceability tuple, and the frozen `pricingSnapshotRef`; **Billing values it from the ref** (never from a live catalog read; SEAMS SUB-B8) and posts. The **dedup is owned here and is per phase-entry occurrence, not per subscription lifetime** (2026-09-20, pricing D-375 / `06-consumer-contracts.md` `inst-pe-*`; supersedes the `(subscriptionId, priceId)` lifetime key): this gear mints a durable **`phaseEntryId`** for every *actual* entry into a plan phase — initial entry, trial conversion, a plan change entering the target's phase, a later re-entry — and **reuses it on every retry** of that entry. The dedup identity is `(tenantId, subscriptionId, phaseEntryId, componentOccurrenceId, chargeLineId)`, with the explicit standalone sentinel as `componentOccurrenceId` for a line not bought through a bundle. The frozen `lineVersionId`, `priceId`, `windowId` and quantity **accompany** the occurrence and are **not** part of its identity, so a price publication between an entry and its retry mints no second charge. A retry, a resume within the same phase, and a skipped phase emit nothing; a genuine re-entry emits again (AC 30); a conversion charge blocked by a payment failure emits on the payment-resolved signal (§6.5, SUB-D-20). Coupons/discounts on one-time lines are Billing-side (launch: none).

#### Rating read-model contract

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-contract-rating-read-model`

**Protocol/Format**: composition read models exposing effective `PlanLink`/`AddOn` intervals, `PlanTier` @ `t`, active **plan phase** at `t`, the plan-change `(changeEffectiveAt, changeMode)`, the **committed seat quantity @ `t`** (effective-dated, §6.3 — pricing `quantitySource = subscription_seat_count`), the **`priceEligibility` inputs** (`activatedAt`, the bound `cohort` via the pinned price id), and the per-sale **`brandId`** evaluation context (§6.2, AC 20); ordering shared on `(resourceTenantId, subscriptionId)` with the **pinned `orderingTenantId`** stable across transfers (§6.7, SUB-D-06). The **pricing** read model additionally supplies the `EligibilityExpirySignal` per bound generation, the **SUB-D-17 notice lookahead inputs** (scheduled `PriceWindow` supersessions + the bound generation's `grandfatherUntil` date — the read-time expiry signal alone cannot arm a 30-day-ahead notice; wave-3 review #23), and **as-of-instant reads up to the dwell bound in the past** (SUB-D-20 revival re-resolution; SEAMS SUB-P6). ([Rating PRD](../../rating/docs/PRD.md) §9.2 "Subscriptions input contract" is the counterpart — its field list names the seat count and `priceEligibility` inputs explicitly; the two lists MUST stay mirror-aligned.)

#### Contracts input contract

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-contract-contracts-input`

**Protocol/Format**: from the **contracts** gear ([contracts PRD](../../contracts/docs/PRD.md) §6.2–§6.7): renewal terms (`autoRenew`, term window, notice ladder), grace ladder and regional-template values with statutory minima, ramps, `PriceOverride` windows, the booking instant and the acceptance-required declaration — consumed via its events (`ContractSigned`, `ContractRenewed`, `ContractAmended`, `ContractTerminated`, and the contract-expired event the contracts gear adds) + read models. `ContractSigned` supplies defaults and eligibility only; it creates no subscription (§4). Subscriptions stores **evaluated fields** with provenance at evaluation time and applies a contract's terms only through an explicit binding; unbound subscriptions run on the platform defaults (§6.5 `fr-platform-defaults`).

#### Orders intent contract

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-contract-orders-intents`

**Protocol/Format**: Orders Workflow submits per-line **draft-create** and **activation** intents (two waves), **draft-void** and **activated-cancel** compensation intents, and one **change intent** per change order. Every intent carries `orderId`, `orderVersion`, `orderLineId` (per item for a change intent), wave, the process `correlationId`, an opaque caller binding reference, and an idempotency key the caller derives from order, version, line and wave. This gear answers each with a confirmation or a failure echoing that envelope, rejects in-flight duplicates machine-readably, supports withdrawal of an accepted request and status lookup by order identity, and relays the `correlationId` to Policy Engine and OSS (§6.1 `fr-order-originated-create`, `fr-cancel-reasons`, `fr-intent-protocol`; §6.3 `fr-composite-change`). Counterparts: [orders-workflow PRD](../../orders-workflow/docs/PRD.md) `fr-owf-provisioning-intent`, [orders-changes PRD](../../orders-changes/docs/PRD.md) `contract-chg-change-intent`.

#### Usage attribution contract

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-contract-usage-attribution`

**Protocol/Format**: batched as-of read `(tenant, resource_ref | subject_ref, t) → (subscriptionId, lineKey)` over the effective-dated attribution binding, with explicit no-match and ambiguous outcomes, answerable over the Usage Collector replay/retention horizon and the correction window (§6.2 `fr-usage-attribution`). Counterpart: [rating SEAMS](../../rating/docs/SEAMS.md) UC3(b), consumed by the rating usage normalizer.

#### Policy Engine gate contract

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-contract-policy-gate`

**Protocol/Format**: pre-commit allow/deny + `reasonCodes` for every resource-affecting transition; fail-closed on deny or unavailability; post-change confirmations per integration Design (manifest §6).

#### License Manager issuance contract

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-contract-license-manager`

**Protocol/Format**: issuance requests (`issue`, `renew`, `replace`, `suspend`, `reinstate`, `revoke`, `convert`) carrying the subscription id and line key, revision, transition, order reference where present, holder/payer/seller tenants, plan revision or SKU and price binding, resolved grant set, quantity, validity and auto-renew posture, and the commercial event token; License Manager returns the license and pack identities or a refusal reason, and publishes license state and findings per subscription line as events and a read model (License Manager PRD §9.2 `contract-license-subscriptions`; mirrored wording). Replays with the same token return the original result; additive fields only.

#### OSS provisioning contract

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-contract-oss-provisioning`

**Protocol/Format**: provision/deprovision/pause work orders confirmed by events; BSS never mutates OSS topology directly (entitlement materialization is License Manager's, `contract-license-manager`). For data-bearing resources: pause on suspend (never deletion), retention hold with read-only export for the data retention window after cancel, deletion at the window's end confirmed by event, a stop on deletion while a legal hold applies (§6.11 `fr-resource-disposition`), and adoption of existing resources on import with inventory validation (§6.1 `fr-import`).

#### Payments failure-signal contract

- [ ] `p2` - **ID**: `cpt-cf-bss-subscriptions-contract-payments-signals`

**Protocol/Format**: payment pre-check outcomes and retry-exhaustion declarations consumed by the renewal/grace ladder (§6.5); PSP webhooks and dunning handoff payloads are Design scope.

## 10. Use Cases

#### Subscription administration (suspend / resume / cancel)

- [ ] `p2` - **ID**: `cpt-cf-bss-subscriptions-usecase-admin-console`

**Actor**: `cpt-cf-bss-subscriptions-actor-partner-admin`

**Preconditions**:
- An existing subscription within the admin's (delegated) scope.

**Main Flow**:
1. Open the subscription.
2. Request suspend / resume / cancel.
3. Track the `TransitionRequest` outcome (pending → approved → applied).
4. Perform bulk operations (batch create/update/cancel) — async with status tracking.
5. Reconcile recurring charges against Billing postings (§17.1).

**Postconditions**:
- Committed transition with Policy pre-check, OSS confirmation where required, entitlements aligned, events emitted.

**Alternative Flows**:
- **Policy deny**: state unchanged; deny + `reasonCodes` surfaced.
- **Missing delegation proof** (cross-tenant): rejected; audit records the attempt.

#### Plan change with proration preview

- [ ] `p2` - **ID**: `cpt-cf-bss-subscriptions-usecase-plan-change`

**Actor**: `cpt-cf-bss-subscriptions-actor-customer`

**Preconditions**:
- An `active` subscription; a published target plan.

**Main Flow**:
1. Select the target plan.
2. Preview the proration charge (calculation authority: the Rating gear's evaluation; surfaced via the preview owner defined in Design).
3. Confirm effective timing (`immediate` / `next-cycle` / `end-of-term`).

**Postconditions**:
- Scheduled or immediate `PlanLink` boundary; `SubscriptionPlanChanged` emitted with `(changeEffectiveAt, changeMode)`.

**Alternative Flows**:
- **Backdated `effectiveFrom` contradicting a posted invoice**: rejected with a clear reason → adjustment path (AC 6).

#### Renewal monitoring and dunning export

- [ ] `p2` - **ID**: `cpt-cf-bss-subscriptions-usecase-renewal-monitoring`

**Actor**: `cpt-cf-bss-subscriptions-actor-finance-analyst`

**Preconditions**:
- Renewal jobs running; some subscriptions in grace.

**Main Flow**:
1. Filter failing renewals (grace state, `graceEndsAt`, ladder variant).
2. Export the audit trail for dunning/compliance.

**Postconditions**:
- Auditable dunning workflow input; no state mutation.

#### Entitlement configuration and monitoring

- [ ] `p2` - **ID**: `cpt-cf-bss-subscriptions-usecase-entitlement-dashboard`

**Actor**: `cpt-cf-bss-subscriptions-actor-product-manager`

**Preconditions**:
- Published plans with grant sets; active subscriptions.

**Main Flow**:
1. Configure entitlement templates for plans (authored as catalog grant sets; per-phase where phased).
2. Monitor the licenses, packs and limits License Manager issued for each subscription and the quota state `quota-enforcement` reports (§6.9).
3. Review compliance/violations; export entitlement reports.

**Postconditions**:
- Template changes flow through catalog publish; monitoring here is read-only.

#### Order fulfillment spawns subscriptions

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-usecase-order-fulfillment`

**Actor**: `cpt-cf-bss-subscriptions-actor-orders`

**Preconditions**:
- An approved `new_sale` order with N lines; payment authorization and, where required, buyer acceptance recorded on the order.

**Main Flow**:
1. Orders Workflow reads overlap occupancy for every line's key and the payer.
2. Wave 1: one draft-create intent per line, with the order reference, the line's add-ons and quantity; each subscription is created in `draft`.
3. Wave 2, once every create has succeeded and the latest service date is reached: one activation intent per line; the start is stamped at the activate commit; this gear binds `(currency, region)`, enforces the overlap rule atomically, runs the Policy/OSS legs and emits any one-time fact.
4. Each confirmation echoes the intent envelope; Workflow acknowledges the order `completed` with the line → subscription mapping.

**Postconditions**:
- N `active` subscriptions, each carrying its order reference; billable facts trace to the order line.

**Alternative Flows**:
- **Create fails before any activation**: Workflow voids the drafts; no billable fact exists.
- **Activation fails after others activated**: Workflow cancels the activated ones with reason `order_compensation`; no ETF or credit is derived; posted one-time facts are reversed in the billing chain.
- **Market divergence or overlap collision at activation**: the line is rejected with a machine-readable reason; Workflow compensates.

#### Change order increases a running subscription

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-usecase-change-order`

**Actor**: `cpt-cf-bss-subscriptions-actor-orders`

**Preconditions**:
- An `active` target subscription; an approved change order with quantity increases and/or component additions against it.

**Main Flow**:
1. Orders Lifecycle reads the target's composition and revision for its delta gate.
2. Orders Workflow submits one `changeComposition` intent carrying every line and the expected revision.
3. This gear checks the revision, non-terminality, add-on bounds and cross-subscription overlap inside one commit and applies every item at its effective date.

**Postconditions**:
- The same subscription carries the increases; composition-changing events carry the per-line order reference.

**Alternative Flows**:
- **Revision mismatch or any failing check**: rejected as a whole with a machine-readable reason; the subscription is unchanged; the order is held for operator retry or cancel.

## 11. User Interaction and Design

| **Interface Name** | **Role** | **Steps** | **Mockup Screen** |
|--------------------|----------|-----------|-------------------|
| Subscription admin console | As a Partner Admin, I want to suspend, resume, or cancel a customer's subscription and track `TransitionRequest` outcomes so that I can respond to operational and payment issues without requiring engineering involvement | 1. Open subscription<br>2. Request suspend/resume/cancel<br>3. Track TransitionRequest | — |
| Plan change wizard | As a Customer or Partner Admin, I want to preview the proration charge before confirming a plan change so that I can avoid unexpected charges | 1. Select target plan<br>2. Preview proration (charge-preview owner per Design; calculation authority = Rating gear)<br>3. Confirm effective timing | — |
| Renewal status | As a Finance analyst, I want to filter failing renewals and export the audit trail so that I can manage dunning workflows and compliance reporting | 1. Filter failing renewals<br>2. Export audit trail | — |
| Customer self-service portal | As an End Customer, I want to view my subscription status, monitor usage against my quotas, and request changes or cancellation so that I control my service access independently | 1. View status & details<br>2. Monitor usage vs quotas<br>3. Request upgrade/downgrade or cancel<br>4. View history & billing info | — |
| Entitlement dashboard | As a Product Manager or Entitlement Admin, I want to configure entitlement templates and monitor issued licenses and quota usage so that I can manage product access controls effectively | 1. Configure templates (catalog grant sets)<br>2. Monitor issued licenses (License Manager) and quotas (quota-enforcement)<br>3. Review compliance<br>4. Export reports | — |

## 12. Acceptance Criteria

> **As a** commercial platform **I want** governed subscription lifecycles **so that** revenue, entitlements, and invoices stay consistent across tenants and channels.

### Lifecycle and guards

**1. Policy gate on resource-affecting transitions**
- **Given** a transition that changes entitlements or provisioned resources
- **When** Subscriptions processes the request
- **Then** a Policy Engine **pre-check** MUST complete before commit
- **And** on deny the subscription state MUST NOT change

**2. Idempotent transitions**
- **Given** two identical requests with the same `(subscriptionId, idempotencyKey)`
- **When** both are processed
- **Then** exactly one durable effect MUST result

**3. Ordering**
- **Given** multiple events for the same subscription
- **When** processed by consumers that require ordering
- **Then** ordering within `(tenantId, subscriptionId)` MUST be preserved

### Composition and billing

**4. Plan change coherence**
- **Given** a plan change with `effectiveFrom`
- **When** Rating and Billing evaluate charges for period `P`
- **Then** both MUST use the same **effective PlanLink set** for `P` (via read models + snapshots)

**5. Recurring idempotency**
- **Given** a closed billing period for `subscriptionId` carrying a plan line and add-on lines
- **When** the bill run retries
- **Then** at most one recurring **`BillableItem`** per `(subscriptionId, billing period, lineKey)` MUST be posted — the key is three-part (SUB-D-19; re-scoped 2026-08-01, wave-3 review #7: the old two-part criterion, enforced as written, would reject the add-on component's fact — the exact F-08-1 defect), each component's fact idempotent on its own key, consistent with AC 27

**6. Backdated plan link rejected when invoices would be contradicted**
- **Given** a backdated **`effectiveFrom`** for a plan change that falls within an **already-posted invoice** period
- **When** the change is submitted
- **Then** the system MUST reject it with a clear reason
- **And** the operator MUST be directed to use the **adjustment** path instead

### Renewal and activation eligibility

**7. Failed renewal**
- **Given** a renewal attempt with failed payment per policy
- **When** the renewal outcome is applied
- **Then** the subscription MUST remain **`active`** during the grace window or transition to **`suspended`** / **`cancelled`** per the published Contract and §6.5 grace policy (grace is a remediation window, **not** a status)
- **And** MUST emit auditable events for downstream dunning

**8. Overlap conflict**
- **Given** a product rule forbidding duplicate active subscriptions for a scope
- **When** activation would violate the rule
- **Then** activation MUST be rejected with a clear, actionable reason

### Versioning, multi-tenant, and events

**9. Snapshot discipline for bill run reproducibility**
- **Given** a billing period `P` for which recurring charges were generated using **PlanLink** set **`L`**
- **When** the bill run is replayed after a plan change that took effect **after** `P`
- **Then** the replay MUST produce the **same** charges using **PlanLink** set **`L`** (via snapshot refs) without re-consulting mutable catalog state

**10. Delegation proof for cross-tenant operation**
- **Given** an admin action on a subscription owned by Tenant **A**, performed by an actor in Tenant **B**'s organizational scope
- **When** the action is processed
- **Then** the audit record MUST include an explicit **delegation proof** reference
- **And** the action MUST be rejected if no valid delegation exists

**11. Event payload completeness**
- **Given** any subscription lifecycle event from the §6.7 producer list that **Rating** or **Billing** consumes
- **When** the event is published
- **Then** the payload MUST be sufficient for **idempotent** consumer handling of the declared commercial effect **without** relying on undocumented side channels
- **And** for **composition-changing** events (including plan changes), the payload MUST be sufficient for **Rating** and **Billing** to apply the **same** effective commercial snapshot intent for the affected period
- **And** concrete **CloudEvents** attributes, extensions, and required-field matrices MUST appear only in **`DESIGN-subscriptions-*`**

**12. Entitlement posture and issuance request on subscription transitions**
- **Given** a successful **resource-affecting** subscription transition whose outcome changes what the subscription confers
- **When** the transition is committed
- **Then** Subscriptions MUST compute the new posture and submit the matching issuance request to License Manager with the commercial event token
- **And** MUST emit `EntitlementPostureChanged`; the materialization events are License Manager's

### Entitlements and trials

**13. Issuance request idempotency and hand-off**
- **Given** a committed transition with commercial event token T
- **When** the issuance request is delivered to License Manager twice, or retried after a timeout
- **Then** exactly one license or pack MUST exist for T and the replay MUST return the original identities
- **And** the first submission MUST happen within the §7.1 hand-off budget

**14. License state visible; pending issuance is an exception**
- **Given** an activated subscription whose issuance request License Manager has not acknowledged within the §7.1 tolerance
- **When** the subscription is read
- **Then** the read model MUST show the issuance as pending and an auditable exception MUST exist
- **And** the subscription MUST NOT be presented as entitled until License Manager reports the license or pack as issued

**15. Entitlement update on plan change**
- **Given** a committed upgrade/downgrade with a target grant set
- **When** the change takes effect (per `changeMode`)
- **Then** a `replace` issuance request carrying the target set MUST be submitted to License Manager with immediate or end-of-cycle effective dates matching the change boundary

**16. Trial auto-conversion continuity**
- **Given** a trial subscription with a payment method on file reaching trial end
- **When** conversion runs
- **Then** the phase MUST advance per `convertsToPhaseId` with entitlement continuity (no access gap) and exactly one conversion (idempotent)
- **And** a payment failure at conversion MUST enter the §6.5 grace ladder

**17. Early conversion (`convertTrial`)**
- **Given** an active trial and an explicit `convertTrial` request
- **When** it is committed
- **Then** the phase boundary MUST move to `now`, the boundary instant MUST be consumed by the rating gear like a plan-change boundary, entitlements MUST be re-issued per the target phase, and a first-class conversion event MUST be emitted

**18. Trial expiry without conversion**
- **Given** a trial reaching expiry without conversion
- **When** the end action runs
- **Then** the subscription MUST follow the configured normal transition (no bespoke terminal status), entitlements MUST be removed
- **And** the win-back hook event MAY be emitted (delivery via Notifications)

**19. Renewal notices**
- **Given** an auto-renewing subscription approaching term end
- **When** the configured notice intervals (default 30/14/7/1 days) are reached
- **Then** notice trigger events MUST be emitted for delivery via Notifications
- **And** an opt-out MUST result in scheduled non-renewal at term end, idempotently

**20. Per-sale brand attribution**
- **Given** a subscription created under a storefront brand
- **When** the pricing evaluation context is published
- **Then** the per-sale `brandId` MUST be present so brand-scoped overlays can match (rating §17.1 step 4)

### Scheduled intents, quantity, pause, activation instants

**21. Scheduled end-of-term cancel**
- **Given** a pending `cancelMode = end_of_term` intent on an auto-renewing subscription
- **When** the renewal window arrives
- **Then** no renewal attempt MUST run and no next-term recurring `BillableItem` MUST be emitted
- **And** at the term boundary the subscription MUST transition to `cancelled` with the normal guards and events

**22. Un-scheduling a pending intent**
- **Given** a pending scheduled intent (cancel or resume)
- **When** it is un-scheduled before its effective instant
- **Then** no transition occurs
- **And** both the scheduling and the un-scheduling MUST be auditable events

**23. Quantity change provenance and boundary**
- **Given** an `updateQuantity` request with a `changeMode`
- **When** it is committed
- **Then** the seat count consumed by rating MUST change exactly at the boundary (increases MAY be immediate and prorated; decreases default to next-cycle)
- **And** the composition-changing event MUST satisfy the AC 11 payload rules

**24. Collection pause**
- **Given** an `active` subscription with a `collectionPaused` window
- **When** recurring generation runs for that window
- **Then** no recurring `BillableItem` MUST be posted (suppressed or deferred per policy)
- **And** service and entitlements MUST remain unaffected, with the pause window auditable

**25. Activation instants**
- **Given** an activation under a Contract with acceptance clauses
- **When** lifecycle events are emitted
- **Then** `serviceActivatedAt` MUST be stamped at the activate commit, `contractEffectiveAt` referenced from the Contract, and `customerAcceptedAt` stamped by the acceptance confirmation
- **And** all three MUST ride the ASC input hooks

### Ordering pin, recurring split, void, pause interplay

**26. Ordering stability across ownership transfer**
- **Given** a subscription whose `resourceTenantId` is rebound by a completed ownership transfer
- **When** events before and after the transfer are consumed in stream order
- **Then** all of them MUST share the one partition keyed by the pinned `orderingTenantId` (stamped at creation, immutable)
- **And** `OwnershipTransferCompleted` MUST carry both the old and the new tenant axes on that partition

**27. Recurring period fact priced exactly once**
- **Given** a recurring period cut for `(subscriptionId, billing period, lineKey)` on a subscription carrying a plan line and an add-on line
- **When** the period fact is emitted and priced
- **Then** the fact MUST carry no monetary amount, the rating gear MUST price the recurring component from the frozen snapshot, and the priced line MUST inherit the fact's idempotency key
- **And** at most one priced recurring line per key MUST reach Billing — no second producer of the recurring line exists
- **And** each billable component MUST be cut as its own fact carrying its own `{skuId, planId, priceId}` — a subscription-wide key cannot represent plan plus add-ons (SUB-D-19)

**28. Draft void**
- **Given** a subscription in `draft` that will never be activated
- **When** a `cancel` (void) is submitted
- **Then** the subscription MUST transition `draft → cancelled` through the normal commit path (audited, idempotent, `cancelMode = immediate`)
- **And** no Policy/OSS provisioning leg MUST run (nothing was provisioned)

**29. Renewal collection during a pause window**
- **Given** an auto-renewing subscription with an open `collectionPaused` window covering the renewal collection
- **When** the renewal window arrives
- **Then** the term MUST extend per Contract, but the payment pre-check, grace entry, and dunning handoff MUST NOT run until the pause window ends
- **And** the deferred collection MUST run when the window ends, per the Billing artifact treatment

**30. One-time charge emitted once per phase-entry occurrence** *(SUB-D-24; added 2026-08-01; re-based 2026-09-20, pricing D-375)*
- **Given** a plan carrying a `one_time` charge line in a phase, and a subscription that enters that phase, is retried, suspends and resumes within it, sees a price publication, converts, changes plan, and later re-enters
- **When** each transition commits
- **Then** exactly one amount-less `BillableItemCreated(kind=one_time)` MUST be emitted per `(tenantId, subscriptionId, phaseEntryId, componentOccurrenceId, chargeLineId)` — per the normative vectors: `initial_entry` → emit; `retry`, `resume_same_phase`, `price_publication` on the same `phaseEntryId` → none; `trial_conversion`, `plan_change` and `later_reentry` each carry a **new** `phaseEntryId` → emit; `skipped_phase` → none
- **And** two component occurrences of one bundle entry MUST each emit their own item, and retrying either MUST emit none
- **And** the event MUST stay money-free: it names the line and the entry, and Billing MUST value it from the frozen `pricingSnapshotRef` (a `flat` 1 500 charges 1 500; a `per_unit` rate 250 at quantity 3 charges 750, before tax and rounding), never from a live catalog read

**31. `autoRenew = false` term expiry resolves** *(SUB-D-13; added 2026-08-01)*
- **Given** an `active` subscription with `autoRenew = false`, no pending intent, and no manual `renew`
- **When** `endDate` passes
- **Then** the system MUST fire an end-of-term `cancel` (reason `term_expired`) through the commit path — the subscription never lingers `active` unbilled
- **And** a transient firing failure follows the firing-failure taxonomy, never an ambiguous term

**32. Nonpayment dwell bounds the suspended state** *(SUB-D-16, SUB-D-20, SUB-D-22; added 2026-08-01)*
- **Given** a subscription suspended for nonpayment with its dwell deadline resolved and stored at suspension
- **When** the customer pays inside the dwell
- **Then** the revival MUST re-run the never-succeeded term extension backdated to the old term end, with composition, payer, and eligibility resolved **as of what the backdated term describes**, and then `resume`
- **When** the (hold-adjusted) deadline passes with no payment resolution
- **Then** the system MUST commit a `cancel` (reason `nonpayment_exhausted`) — re-derived daily until it commits, with no OSS-blocking leg against already-deprovisioned resources
- **And** payment resolution or any committed exit from `suspended` MUST void the deadline first

**33. Price-change renewal notice armed ahead** *(SUB-D-17; added 2026-08-01)*
- **Given** a bound price row whose scheduled supersession or `grandfatherUntil` takes effect at/before the next renewal instant
- **When** the 30-day notice trigger fires
- **Then** `SubscriptionRenewalNoticeDue` MUST carry `priceChangePending = true`, derived from the pricing lookahead inputs (scheduled `PriceWindow`s + the generation's `grandfatherUntil` date), with no amount computed in this gear

**34. ETF derivation is reason-scoped and joins on the carried keys** *(SUB-D-18/25; added 2026-08-01)*
- **Given** cancellations with reasons `customer`, `term_expired`, and `nonpayment_exhausted`
- **When** Billing derives the Contracts-defined commercial consequence from `SubscriptionCancelled`
- **Then** only the `customer` (and `operator`) cancel MAY yield an ETF or unused-portion credit; `term_expired`, `nonpayment_exhausted`, `saga_superseded`, and `order_compensation` MUST NOT
- **And** the derivation MUST join on the event's term window + billing-period identity, never re-reading the aggregate at posting time

### Orders, contracts, and usage attribution

**35. Order-originated two-phase acquisition**
- **Given** an approved order whose two lines are submitted as draft-create intents carrying the order reference
- **When** both creates commit
- **Then** two subscriptions MUST exist in `draft`, each carrying its `orderId`, `orderVersion` and `orderLineId`, with no Policy/OSS leg run and no billable fact emitted
- **And** a repeated draft-create intent with the same caller-derived key MUST return the same subscription, while an intent derived from a later `orderVersion` MUST NOT be absorbed as its duplicate

**36. Activation starts at the activate commit**
- **Given** an order line whose quoted service date has passed while the activation wave waited for another line
- **When** the activation intent commits
- **Then** the subscription's start (`serviceActivatedAt`), term and first recurring period MUST begin at the commit instant — never at the quoted date and never at an instant supplied by the caller
- **And** any one-time fact MUST be emitted at this activation, not before

**37. Market divergence at activation**
- **Given** a draft whose line was priced in one currency and region, and a payer whose commercial profile now binds another
- **When** the activation intent is processed
- **Then** activation MUST be rejected with the machine-readable market-divergence reason and the subscription MUST stay in `draft`

**38. Order compensation cancel derives no money**
- **Given** an order-activated subscription whose order fulfillment later failed
- **When** it is cancelled with reason `order_compensation`
- **Then** no early-termination fee or unused-portion credit MUST be derived
- **And** a one-time fact already emitted MUST stay posted for Billing to reverse

**39. Change intent applies all-or-nothing**
- **Given** a `changeComposition` request with a quantity increase and a component addition, whose added component would breach the plan's add-on maximum
- **When** the request is processed
- **Then** it MUST be rejected as a whole with a machine-readable reason, the subscription `version` MUST be unchanged, and neither item MUST take effect
- **And** a request whose expected revision differs from the current `version` MUST be rejected the same way

**40. Overlap self-exemption and atomic enforcement**
- **Given** a key with `maxConcurrentActive = 1`, a subscription holding it, and a second draft on the same payer and key
- **When** a `changeComposition` adds a component to the first subscription, and separately the second draft is activated
- **Then** the change MUST NOT be rejected for colliding with its own target
- **And** the activation MUST be rejected with the overlap reason — including when two activations on the key race, of which at most one may commit
- **And** the occupancy read for the key MUST report `activeCount = 1`, `maxConcurrentActive = 1` and its provenance

**41. Resubmit, withdrawal, status read**
- **Given** an accepted activation intent whose OSS work order is outstanding (`approved`)
- **When** the caller resubmits it with the same key, then withdraws it, then reads it by `orderId` + `orderVersion` + `orderLineId` + wave
- **Then** the resubmit MUST return the existing request with status `approved` and no second effect
- **And** the withdrawal MUST cancel the work order with a compensating deprovision, leave the subscription in `draft`, and end the request as `failed` with reason `withdrawn`
- **And** the read MUST return that outcome

**42. Identity echo and correlation**
- **Given** any intent carrying a `correlationId` and an opaque binding reference
- **When** its confirmation or failure is published
- **Then** it MUST echo the order reference, wave, idempotency key, `correlationId`, binding reference and asserting principal
- **And** the `correlationId` MUST appear on the Policy Engine and OSS legs driven for that request

**43. Uncontracted subscription runs on platform defaults**
- **Given** a subscription with no contract binding, and later a contract signed for the same payer without a binding to this subscription
- **When** renewal, notice and grace are evaluated
- **Then** the platform defaults MUST govern, recorded with provenance `platform_default`
- **And** the signed contract MUST NOT change this subscription's terms until an explicit binding is recorded

**44. Usage attribution as of the consumption instant**
- **Given** a resource reference provisioned under subscription A's add-on line, deprovisioned, and later provisioned again under subscription B's plan line
- **When** Rating reads the attribution for a usage record stamped inside A's interval, and for one with an unknown resource reference
- **Then** the first MUST resolve to subscription A and that add-on's `lineKey`
- **And** the second MUST return an explicit no-match, never a guess

**45. Trial without a payment method does not convert**
- **Given** a trial whose target phase is paid and no payment method on file at trial end
- **When** conversion runs
- **Then** the phase boundary MUST NOT advance and no paid-phase entitlement MUST be issued; the trial MUST follow its expiry path
- **And** with a valid method on file, a payment failure at conversion MUST enter the §6.5 grace ladder instead (AC 16)

### Data, terms, co-termination, and import

**46. Suspend keeps customer data**
- **Given** an `active` subscription with data-bearing resources
- **When** it is suspended for nonpayment and stays suspended for 89 days
- **Then** its data-bearing resources MUST still exist, paused and inaccessible
- **And** `resume` after payment MUST restore access to the same resources

**47. Cancel opens a retention window before deletion**
- **Given** a subscription cancelled with reason `customer` under the platform defaults
- **When** the cancel takes effect
- **Then** entitlements MUST be revoked and a 30-day retention window MUST open, with read-only export available
- **And** pre-deletion notices MUST fire 7 days and 1 day before the window ends
- **And** at the window's end OSS MUST delete the resources and confirm by event, and the purge MUST be audited
- **And** a legal hold placed during the window MUST stop the deletion until released

**48. Record retention and erasure**
- **Given** a subscription cancelled more than 90 days ago whose data retention window has ended
- **When** the retention job runs
- **Then** the subscription MUST move to `archived`
- **And** its records MUST NOT be purged before the configured financial-record retention period, nor while a legal hold applies
- **And** an erasure request MUST tombstone personal-data references without changing the records used to reproduce posted invoices

**49. Add-ons co-terminate with the base subscription**
- **Given** an annual subscription and an add-on attached in its fourth month
- **When** the term ends and auto-renewal runs
- **Then** the add-on's first interval MUST end at the subscription's term end, prorated by the Rating gear
- **And** the renewal MUST extend the plan line and the add-on together

**50. New subscription with a co-term date**
- **Given** a contract whose term ends in five months, and a new subscription bound to it with a one-year term length
- **When** the subscription is created and activated
- **Then** its first term MUST end at the contract term end, prorated by the Rating gear
- **And** its next terms MUST be one year long, aligned to that date

**51. Term from the order line; evergreen term end**
- **Given** an uncontracted order line carrying a one-year term, and separately an uncontracted evergreen subscription billed monthly
- **When** the first subscription is created from the line, and the second receives an `end_of_term` cancel
- **Then** the first MUST carry a one-year fixed term
- **And** the second MUST be cancelled at the end of its current monthly billing period

**52. Import adopts a live subscription without re-provisioning or re-billing**
- **Given** a legacy subscription with running resources, billed through the end of last month
- **When** it is imported
- **Then** it MUST be created `active` with its original start and current term, adopting the listed resources with no provisioning work order
- **And** no one-time or setup fact MUST be emitted, and the first recurring fact MUST start at the billed-through instant
- **And** importing the same legacy identifier again MUST return the existing subscription

## 13. Dependencies

| Dependency | Description | Criticality |
|------------|-------------|-------------|
| AMS / OSS (tenant identity & hierarchy) | Tenant identity, `resourceTenantId` topology references, account/OrgTier context, delegation-proof backbone | `p1` |
| Catalog registry (Product & SKU) | Published `skuId`, `PlanTier` taxonomy, `CatalogVersion`, `catalogSubscriptionProductKey` for the overlap rule (not yet exposed — SUB-G1); the **products** gear, `gears/bss/products/docs/PRD.md` (vendored 2026-07-16) | `p1` |
| Pricing (Product Catalog) | Published `planId`, `PriceWindow` linkage, price snapshot refs, trial offers ([pricing PRD](../../pricing/docs/PRD.md)) | `p1` |
| Rating (evaluation core + pipeline) | Consumes composition read models + `(changeEffectiveAt, changeMode)`; owns proration math and usage slicing ([rating PRD](../../rating/docs/PRD.md)) | `p1` |
| Billing & Invoicing | Ingests recurring `BillableItem`s; posts immutable invoices; adjustments/credit/debit notes; dunning execution | `p1` |
| Contracts & Agreements | The **contracts** gear (first draft): renewal terms and notice ladder, grace ladder / regional templates, ramps, `PriceOverride` windows, booking instant and acceptance declaration; optional per subscription — platform defaults govern where none is bound | `p1` |
| Orders (Lifecycle + Workflow + Change Orders) | Caller of record for commercially initiated creates and increases: two-phase acquisition intents, compensation intents, change intents; consumer of the occupancy, composition and status reads ([orders-lifecycle](../../orders-lifecycle/docs/PRD.md), [orders-workflow](../../orders-workflow/docs/PRD.md), [orders-changes](../../orders-changes/docs/PRD.md)) | `p1` |
| Usage Collector (via Rating) | Usage records without commercial identity; attribution resolved through this gear's read (§6.2 `fr-usage-attribution`) | `p1` |
| Policy Engine | Fail-closed allow/deny + `reasonCodes` for resource-affecting transitions | `p1` |
| License Manager | Materializes the entitlement posture: issues, renews, replaces, suspends and revokes the licenses, packs and limits that back a subscription on this gear's idempotent request; reports license state and findings back (upstream `PRD-license-manager-202610021000`) | `p1` |
| OSS Provisioning | Provision/deprovision/pause execution confirmed by events; pause without deletion on suspend; retention hold, export and confirmed deletion after cancel; adoption of existing resources on import | `p1` |
| Payments (PSP) | Payment pre-check + retry-exhaustion signals for the grace ladder; authorization at renewal/trial conversion | `p2` |
| Notifications / Comms | Delivery of renewal notices, trial-expiry and win-back hooks (triggers owned here) | `p2` |
| BSS Architecture Manifest | §4.3 primary; §4.1/§4.2/§4.4 contracts; §4.6 renewals; §2.1.3 identities; §6 gates | `p1` |

## 14. Assumptions

- SLA numbers in §7.1 are working baselines from the predecessor PRD pending the program NFR workshop; the workshop overrides on conflict.
- In this repository Plan&Price and Tariffs+Rating are the **pricing** and **rating** gears (originally vendored from upstream; upstream not maintained). The Product&SKU registry is the **products** gear (vendored 2026-07-16); it does not yet expose `catalogSubscriptionProductKey` (seam SUB-G1).
- The **contracts** gear authors renewal, notice and grace terms and regional templates (`fr-renewal-terms`, `fr-grace-regional`) but is a first draft with no acceptance criteria; until it ships, every subscription runs on the platform defaults (§6.5), which this PRD treats as a first-class state.
- The order path may ship after this gear. Until it is live in a deployment, `create` is called directly (§6.1 `fr-order-originated-create` item 9); the order-path contract is specified now because the cancel-reason set and the create dedup rule are breaking to change later.
- Design closes trial attribute/event naming, overlap **dimension** binding, and Payments/Billing integration payloads (PSP webhooks, dunning handoff) consistent with the §6.5 grace ladder.
- Trials remain representable without a `trial` status unless the BSS manifest enum is amended.
- PostgreSQL is sufficient for subscription/entitlement state at launch; re-evaluate at the 100K+/tenant scale target (Design).
- Eventual consistency between subscription state and downstream read models is acceptable — except the entitlement posture hand-off to License Manager, which follows the §7.1 `nfr-entitlement-handoff` budget.
- Payment authorization/capture arrives via the future Payments module; this gear only triggers requests and consumes outcome signals.

## 15. Open Questions

| **Question** | **Owner** | **Target Date** | **Answer** | **Date Answered** |
|--------------|-----------|-----------------|------------|-------------------|
| Is **`trial`** a `Subscription.status`? | Product / Architecture | — | **No** — trials use **attributes / `PlanLink` / contract+Catalog** on manifest statuses (§6.1). A manifest `trial` status would require manifest + Design change first. | 2026-05-12 |
| Trial **commercial pattern** (trial-only SKU vs attribute-only vs hybrid) | Product / Catalog | — | **Resolved:** **Catalog-first** trial definition + optional **Contract** clauses + **Subscription** evaluated attributes (§6.1). **Amended 2026-09-29:** attribute-only trials are not permitted — no Contracts artifact authors trial clauses. | 2026-05-12 |
| Default cardinality: overlapping subscriptions allowed? | Product / Catalog | — | **Resolved:** default **one** `active` per **`(payerTenantId, catalogSubscriptionProductKey)`** unless Catalog/Contract sets **`maxConcurrentActive` > 1** or extra scope dimensions (§6.3). The occupancy read returns the effective limit with provenance, the default included, so callers never assume one (2026-09-29). | 2026-05-12 |
| Overlap-key dimension on the partner path: one payer buying the same product for several customer tenants collides on the default key `(payerTenantId, catalogSubscriptionProductKey)`. Does the default key gain `resourceTenantId`, or does the partner path rely on `maxConcurrentActive` > 1? | Architecture / Product (with Orders) | TBD | Raised by orders-lifecycle §15; Orders adopts the answer by reference and MUST NOT fork the key. | — |
| Subscription composition granularity: one order line → one subscription is fixed here; a tenant with several enabled products as **one** subscription with a multi-product entitlement set is not expressible. Keep 1:1? | Architecture (with Orders) | TBD | Raised by orders-lifecycle §15; resolve here first, then reflect in Orders. | — |
| Default auto-renewal and default term for an **uncontracted** subscription (§6.5 `fr-platform-defaults`) | Product | — | **Resolved:** evergreen with auto-renewal on; the order line's term overrides the default (§6.5 `fr-term-source`). | 2026-10-01 |
| **Two-tier (wholesale/retail) billing** for resellers: how one subscription or usage stream yields a platform→reseller wholesale charge and a reseller→customer retail charge, and which payer rule wins — the order-supplied `payerTenantId` here or the ledger's "nearest `self_managed` ancestor-or-self" (ledger design/01a) | Product + Architecture (with Ledger; a partner-settlement PRD to be authored) | TBD | Not specified anywhere; the ledger defers inter-tenant settlement to a PRD that does not exist. | — |
| **External billing mode**: a provider keeps its own billing system (e.g. WHMCS, HostBill) as the source of truth while this gear holds entitlements and usage; outbound webhooks / partner event API (signing, retry, replay, versioning) and inbound sync with identifier mapping | Product + Architecture (with event-broker) | TBD | Manifest §6 allows an external system "acting as the authoritative source"; no gear specifies the mode or webhooks. | — |
| **Account Management linkage**: tenant delete precondition "no non-terminal subscriptions" for payer and resource tenants; effect of tenant and partner suspension on subscriptions; implementation of the manifest offboarding order | Architecture (Account Management + Subscriptions) | TBD | Account Management today checks only Resource Group associations before deleting a tenant. | — |
| **Dunning outside renewal**: path to grace or suspension for an unpaid recurring invoice of an evergreen subscription and for an unpaid in-arrears usage invoice; who executes dunning | Billing (no gear yet) + Product | TBD | The §6.5 ladder triggers only on renewal. | — |
| **Payment method binding**: per account or per subscription; meaning of "valid"; pay-by-invoice customers and resellers versus the auto-renewal pre-check | Payments (no gear yet) + Product | TBD | — | — |
| **Money on cancellation for uncontracted subscriptions**: early-termination fee, unused-portion refund or credit, credit application; pricing D-49 ("no credits at launch") | Product + Billing | TBD | Contracts authors no fee or refund terms; the platform defaults define none. | — |
| **Consumer protection**: cooling-off / withdrawal right, reminder before a trial converts to paid, cancellation accepted even while the Policy Engine is unavailable, consumer-vs-business flag | Product + Legal | TBD | — | — |
| **Availability and recovery NFRs**: availability SLO (manifest: Catalog/Subscriptions 99.5%), RPO/RTO (manifest: RPO ≤ 5 min, RTO ≤ 60 min per region), backup and tenant-scoped restore, completion windows for renewal, notice and retention jobs | Architecture (NFR workshop) | TBD | Manifest values exist; this PRD does not adopt them yet. | — |
| **Actor × operation permission matrix**: what each actor (customer, partner admin, seller operator, finance, platform operator, system) may do and see, including the audited operator override that restores unpaid service | Product + Security | TBD | Orders Lifecycle §6.6 is the template. | — |
| **Enforcement ownership** with the platform license-enforcement and quota-enforcement gears: who counts quota, who issues licence packs, how suspend maps to them | Architecture | TBD | Answered — Subscriptions decides the posture and submits an idempotent issuance request; **License Manager** (upstream `PRD-license-manager-202610021000`) materializes licenses, packs and limits for both the platform and installed products; **license-enforcement** answers checks; **quota-enforcement** counts; suspend on the grace ladder maps to a `suspend` request (§6.9, SEAMS section J). The platform `license-enforcement` PRD still names Subscriptions as the pack-issuing actor — a one-line alignment to request from its owners. | 2026-10-02 |
| **Data residency** of subscription stores, check-surface replicas and events | Architecture + Legal | TBD | The ledger defers residency post-MVP; this PRD should state the same explicitly or adopt a rule. | — |
| **Re-attaching retained resources** to a new subscription within the data retention window (reinstatement after a mistaken or regretted cancel) | Product | TBD | §6.11 keeps the data; the commercial path back is open. | — |
| **Bill-cycle day per payer**: aligning billing period boundaries (not only term ends) across a payer's subscriptions | Product + Pricing + Billing | TBD | §6.5 `fr-co-termination` aligns term ends only; period boundaries follow each price's `billingAnchorPolicy`. | — |
| **Re-aligning running subscriptions to a co-term date**: when a contract is bound later or a payer enables co-terming, are existing subscriptions realigned — at their next renewal (shortened or lengthened next term) or immediately (a prorated adjustment of the current term)? | Product (with Rating and Contracts) | TBD | §6.5 `fr-co-termination` covers new subscriptions, add-ons and change-order items only. | — |
| Binding a contract to a **live** subscription: does it change the in-flight term or only the next one? | Product (with Contracts) | TBD | Mirrors contracts §15; determines whether a binding is additive or behaviour-changing. | — |
| Effect of `ContractTerminated` / contract expiry on a bound subscription: end-of-term cancel, or reversion to platform defaults? | Product (with Contracts) | Before the contracts gear publishes those events | §6.5 `fr-platform-defaults` item 4 requires the events to be consumed and recorded; the behaviour is open. | — |
| Buyer-decided trial conversion and re-negotiated renewal: order-driven (price pin, approval, booking record) or direct? | Product (with Orders) | TBD | Raised by orders-lifecycle §15; automatic conversion and clockwork renewal stay direct. | — |
| Change intent with items at **different future dates** (§6.3 `fr-composite-change` item 4): acceptance is one commit, but each dated item executes later under the full guard set (SUB-D-01) and can fail then — accept per-item execution, or require one effective date per change intent? | Product (with Orders) | TBD | Change Orders allows a date per line and requires all-or-nothing application; the two meet only if the dates coincide. | — |
| Failed renewal **grace** (duration, recurring posture, Contract SoR, suspension triggers) | Product / Contracts | — | **Resolved in PRD** — §6.5 grace policy (7-day default, paused next-term recurring, Contract SoR + evaluated fields, hybrid exit trigger). | 2026-05-12 |
| Legacy upstream PR #154 review items (canonical `refs`, HTTP header-name leakage, AC 7 "grace"-≠-status wording, proration ownership) | — | — | **Resolved / N/A.** All were addressed in this copy when the doc was brought here (canonical `refs`, header-name leakage §9.1, AC 7 wording, proration ownership §6.3); `gears-rust` is now canonical and upstream is not maintained, so there is no sync obligation. The one substantive item that remained a **live cross-PRD dependency** — Contracts owning the grace/regional-template SoR — is now authored by the contracts gear (`fr-grace-regional`, first draft); its completion is tracked as risk (§16, seam SUB-C1). | 2026-07-15 |
| `convertTrial` / `updateQuantity` / `changeComposition` / the SUB-D-08 set (`renew`, `unschedule`, `pauseCollection`, `resumeCollection`, `confirmAcceptance`, `extendTrial`, `archive`) as `TransitionRequest.type` values + the scheduled-intent envelope (`cancelMode`, `resumeAt`) + the order reference on `create`, the closed cancel-reason set incl. `order_compensation`, and the `import` creation edge into `active`/`suspended` — manifest §4.3 alignment | Architecture / manifest owners | TBD | Proposed by §6.1 / §6.3 / §6.4 / §6.5 / §6.10 (this repo); needs manifest alignment like any type or envelope addition. | — |
| Direct `create` alongside the order path (manifest §4.6.1: "only a fulfilled Order spawns the Subscription") | Architecture / manifest owners | TBD | §6.1 `fr-order-originated-create` keeps the direct path for system, migration and operator creates and for pre-Orders deployments; the manifest note needs the same carve-out. | — |
| Transfer billing boundary (immediate vs next-cycle payer rebind; mid-period payer split) | Product / Billing | TBD | SUB-D-06 pins the ordering key; the collection-side boundary of a payer rebind is a Billing/Product call (design slice 07 defaults to next-cycle). | — |
| Brand overlay source discrepancy: rating PRD matches `brand` on Plan/SKU `brandId`, this PRD publishes the per-sale `brandId` (§6.2, AC 20) | Rating / Subscriptions | TBD | Pin with rating which source feeds step-4 brand matching (seam SUB-R5); AC 20 is not implementable while the two disagree. | — |
| Draft retention TTL (auto-void of abandoned drafts) | Product | TBD | SUB-D-11 adds the `draft → cancelled` edge; **platform default 90 days since 2026-08-01** (SUB-D-11 amendment — the retention job submits the void; tenant-configurable, Product knob refines). | — |
| Scheduled-intent firing grace horizon (retryable-class deadline: `effectiveAt` + horizon) | Product / Ops | TBD | Slice 01 §4.3 taxonomy cites this knob; row added 2026-08-01 (wave-3 review #5). Parked (state-precondition) firings suspend the horizon per SUB-D-23. | — |
| Repeat-trial eligibility (serial re-trials after cancel) | Product / Pricing | TBD | No owner today; the overlap rule blocks only concurrent duplicates. Candidate: pricing trial-offer eligibility window or Contract clause. | — |
| **Free paid-access vector (REVIEW F-06-1, revenue/abuse)** — the three open legs *compose* into an exploit loop: conversion with **no payment method on file** still issues full **paid-phase** entitlements (§6.10), the failure then enters the **7-day paid grace** ladder (§6.5), and serial re-trials are unbounded (row above). trial → convert with no method → 7 days full paid access → cancel → new trial → repeat | Product / Finance | Before trial GA | **Resolved: (c)** — a paid target phase requires a valid payment method before the conversion boundary advances; without one the trial follows its expiry path and no paid-phase entitlement is issued (§6.10 `fr-trial-conversion`, AC 45). This restores the predecessor's "no payment method → expire trial" behaviour lost in the 2026-07-15 consolidation. Options (a) limit serial re-trials and (b) reduced access during grace were not taken; serial re-trial eligibility stays open (row above). | 2026-09-29 |
| Quota-crossing propagation bound (usage → check-state lag; overrun exposure) | OSS / Rating / Product | TBD | Moved — counting and the usage-to-decision lag budget are `quota-enforcement`'s; this gear holds no check state since the re-split (§6.9). | 2026-10-02 |
| Entitlement check staleness budget default (SUB-D-10: last-known-good ≤ 60s on projection outage, then fail-closed — **feature-flag dimension only**) | Product / OSS | TBD | Withdrawn with the check surface (SUB-D-10 withdrawn, re-split 2026-10-02); the staleness budget is `license-enforcement`'s. | 2026-10-02 |
| Quota freshness bound (SUB-D-10 amendment: the quota dimension of a check fails closed to `blocking` beyond it — never a last-known-good quota `allow`; provisional default **10s**) | Product / OSS | TBD | Ratify with the staleness budget before Design lock; slice 05 §4.3 owns the split. | — |
| Ramp authoring in Contracts (committed multi-step schedules; atomic multi-action orders) | Contracts / Design | TBD | SUB-D-04: Contracts authors the committed ramp; Subscriptions executes generated scheduled intents (§6.3); Contracts PRD follow-up. | — |
| Acceptance confirmation flow (who confirms, evidence shape) | Product / Design | TBD | SUB-D-05 fixes the instants as attributes (§6.1); the confirmation operation flow is Design. | — |
| Quota exhausted mid-request: graceful degradation vs hard block | OSS / Design | TBD | §6.9 fixes the check-state semantics; the mid-request instant is OSS/Design. | — |
| Trial extension approval policy (automatic / manual / threshold-based) | Product | TBD | §6.10 requires approval-gating; the policy itself is open. | — |
| Notice intervals & grace durations: partner-configurable vs platform standard | Product / Contracts | TBD | Working default: platform defaults (30/14/7/1 notices; 7-day grace) with Contract/template override within Legal bounds. | — |
| Pause mechanics (pause-day limits, resume proration) | Product / Billing | TBD | Posture decided (SUB-D-03: `collectionPaused` subscription attribute, collection-scoped — §6.4); since 2026-08-01 the limit is **cumulative per term, 90-day platform default** (slice 04 §4.2 — consecutive windows count together, evaluated at submit fail-closed); the knob's final value + resume proration remain open. | — |
| Maximum subscription term length | Product / Contracts | TBD | Impacts Contract templates and renewal windows. | — |
| Entitlement inheritance for bundled subscriptions (parent/child aggregation) | Product / Design | TBD | Bundling lifecycle sync is `p3` scope; aggregation rules undefined. | — |
| Proration credit application policy (immediate credit / next invoice / refund) | Finance / Billing | TBD | `creditOnDowngrade` is published by pricing; the application policy is Billing/Finance. | — |
| Committed usage: Subscriptions or Contracts? | — | — | **Resolved**: commitment pools = Contracts SoR, true-up = rating (T-D-14); this gear keeps hooks only (§2.2). | 2026-07-15 |
| Which subscription models are supported (fixed-term, evergreen, hybrid, prepaid)? | — | — | **Resolved**: all four via composition — terms on Contract, hybrid via pricing hybrid plans, prepaid via the pricing D-43 grant + Billing execution (GA-gated). | 2026-07-15 |

## 16. Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| Suspension vs billing alignment left implicit (manifest risk note) | Revenue leak or double-charge during suspension | §6.4 requires the posture to be explicit product policy in subscription attributes + contract clauses |
| Evergreen renewals / notice periods under-specified (manifest risk note) | Non-compliant auto-renewals; disputes | Contract templates MUST define notice/opt-out behavior (§6.5); Legal bounds on grace overrides; a pending price change at the renewal instant arms the 30-day **commercial** notice from the pricing lookahead inputs (SUB-D-17, AC 33 — mitigation completed 2026-08-01, wave-3 review #23) |
| Contracts gear is a first draft (§1–§6, no acceptance criteria) | Renewal, notice and grace terms have no shipped owner; platform defaults harden into de-facto terms customers rely on | Platform defaults specified as a first-class state with provenance and an explicit-binding rule (§6.5 `fr-platform-defaults`); the in-flight-term migration rule is open (§15) |
| Order-path seams unagreed (SEAMS §I: SUB-O1…O16, CHG-S1…S5) | Orders cannot guarantee atomic fulfillment, no-backdating or retry safety; the cancel-reason set is breaking to change once Billing consumes it | Adopted in §6.1 / §6.3 of this revision; the reason set and the create dedup rule are fixed now, before any consumer |
| Proration ownership conflicts with older upstream Rating/Billing PRD wording | Ambiguous calculation authority ("Billing preview API") | §6.3 ownership split is normative here and matches the local rating gear; preview owner named in Design (§11 wording already neutral) |
| Dunning/PSP integration details deferred to Design | Grace ladder not executable end-to-end at launch | §6.5 fixes product defaults; Design encodes timers + Payments signals before implementation |
| Issuance hand-off lag or refusal: a committed transition whose License Manager request is delayed or refused leaves the customer without entitlements | A service that was bought is not usable, or an activated subscription has no license behind it | `nfr-entitlement-handoff` 5 s / 60 s budget; retry under the same token; pending-issuance exception on the read model and refusal surfaced on the transition request (§6.9) |
| Notifications integration missing at launch | Renewal notices / opt-out windows silently missed | §6.5 triggers are normative; Notifications delivery is a tracked `p2` dependency (§13) |

## 17. Reference Materials

| **Material** | **Link** | **Comments** |
|--------------|----------|--------------|
| BSS Architecture Manifest | `docs/bss/manifest/vz-arch-manifest-bss-only.md` (upstream — **historical, not vendored into this repo**) | §4.3 primary; §4.6 renewals; §6 gates |
| Origin (provenance) | `bitbucket.org/virtuozzocore/vhp-architecture` PR #154, branch VHP-806, commit `4faef39652d0` | Originally vendored 2026-07-15; upstream **not maintained** — `gears-rust` is canonical |
| Subscriptions & Entitlements (predecessor module PRD) | `docs/bss/prd/PRD-subscriptions-entitlements-202601120119/` (upstream, legacy) | **Absorbed into this PRD (2026-07-15)** — §2.2 section map; the upstream copy is legacy provenance (not maintained) |
| Product & SKU Management (Catalog registry §4.1) | **products** gear — `gears/bss/products/docs/PRD.md` (vendored 2026-07-16 from PR #4177) | SoR for Product/SKU/Category/Attribute/`PlanTier`/`CatalogVersion` |
| Plan & Price Modeling (Catalog §4.1) | [pricing PRD](../../pricing/docs/PRD.md) (vendored gear) | Owns `Plan`/`Price`/`PriceWindow` linkage that `PlanLink` resolves |
| Rating (§4.2 — incl. the evaluation core, former "Tariffs") | [rating PRD](../../rating/docs/PRD.md) (vendored gear, consolidated per rating ADR-0002) | Owns proration math, override hierarchy, coupons, FX; consumes `(changeEffectiveAt, changeMode)`; shared ordering key |
| Contracts and Agreements (§4.6) | **contracts** gear — [`gears/bss/contracts/docs/PRD.md`](../../contracts/docs/PRD.md) (first draft; the upstream `PRD-contracts-agreements-202601120119` is legacy provenance) | SoR for renewal terms, notice and grace ladders, regional templates, ramps, booking/acceptance |
| Orders Lifecycle | [`gears/bss/orders-lifecycle/docs/PRD.md`](../../orders-lifecycle/docs/PRD.md) | Order document and state SoR; two-phase atomic fulfillment; boundary rules R1–R5 |
| Orders Workflow | [`gears/bss/orders-workflow/docs/PRD.md`](../../orders-workflow/docs/PRD.md) | Caller of record for order-path intents; retry, sweep and compensation protocol |
| Change Orders | [`gears/bss/orders-changes/docs/PRD.md`](../../orders-changes/docs/PRD.md) | Change intent contract and asks CHG-S1…S5 |
| Billing — Ledger & Balances (§4.4) | **ledger** gear — [`gears/bss/ledger/docs/PRD.md`](../../ledger/docs/PRD.md) (vendored; the `docs/bss/prd/PRD-billing-ledger-balances-202604041200/` path is historical, not vendored here) | Posted-invoice immutability, adjustments, rounding authority |
| Billing module (§4.4) | `docs/bss/prd/PRD-billing-module-202601120119/` (upstream) | Recurring ingestion, dunning execution |

### 17.1 Reconciliation Framework (operational appendix)

Daily/weekly cross-checks carried from the predecessor; concrete owners and schedules are confirmed in Design:

| **Check** | **Source A** | **Source B** | **Frequency** | **Outcome** |
|-----------|--------------|--------------|---------------|-------------|
| Charge coverage | Subscriptions with `nextChargeDate` passed | Billing recurring postings | Daily 02:00 | Identify missed charges or posting failures |
| Entitlement sync | Resolved posture per subscription | Licenses, packs and limits License Manager reports as issued | Daily 04:00 | Detect posture-to-issuance drift (missing, extra or stale issuance) |
| Billing alignment | Subscription charge schedules | Billing ledger entries | Daily 06:00 | Verify charge amounts and periods match |
| Trial conversion | Expired trials with payment method | Converted subscriptions | Daily 08:00 | Track conversion success rate; identify failures |
| Renewal processing | Subscriptions at term end | Renewed subscriptions | Daily after renewal window | Ensure all renewals processed |
| Catalog sync | Subscription plan references | Catalog plan definitions | Weekly | Detect orphaned plans or version mismatches |
