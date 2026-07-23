---
refs:
  - bss/manifest/vz-arch-manifest-bss-only.md
  - bss/prd/PRD-billing-ledger-balances-202604041200
  - bss/prd/PRD-contracts-agreements-202601120119
  - bss/prd/PRD-metering-pricing-module-202601120119
  - bss/prd/PRD-product-catalog-marketplace-202601120119
  - bss/prd/PRD-product-catalog-marketplace-202601120119/UC-effective-dating-price-windows-202601121200.md
  - bss/prd/PRD-product-catalog-marketplace-202601120119/UC-plan-price-modeling-202601121200.md
  - bss/prd/PRD-product-sku-management-202606101924
  - bss/prd/PRD-rating-engine-202604031200
  - bss/prd/PRD-subscriptions-entitlements-202601120119
  - bss/prd/PRD-tariffs-pricing-logic-202604011200
---

# PRD — Plan & Price Modeling

<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Background / Problem Statement](#12-background--problem-statement)
  - [1.3 Goals (Business Outcomes)](#13-goals-business-outcomes)
  - [1.4 Glossary](#14-glossary)
- [2. Architecture Alignment](#2-architecture-alignment)
  - [2.1 Boundaries](#21-boundaries)
  - [2.2 Canonical Scope Key (normative)](#22-canonical-scope-key-normative)
  - [2.3 Predecessor Documents and Scope Migration](#23-predecessor-documents-and-scope-migration)
- [3. Actors](#3-actors)
  - [3.1 Human Actors](#31-human-actors)
  - [3.2 System Actors](#32-system-actors)
- [4. Operational Concept & Environment](#4-operational-concept--environment)
  - [4.1 Module-Specific Environment Constraints](#41-module-specific-environment-constraints)
- [5. Scope](#5-scope)
  - [5.1 In Scope](#51-in-scope)
  - [5.2 Out of Scope](#52-out-of-scope)
- [6. Functional Requirements](#6-functional-requirements)
  - [6.1 Plan Definition and Billing Cycles](#61-plan-definition-and-billing-cycles)
  - [6.2 Price Structure and Model Kinds](#62-price-structure-and-model-kinds)
  - [6.3 Plan Composition, Descriptors, and Phases](#63-plan-composition-descriptors-and-phases)
  - [6.4 Multi-Currency, Regions, and Tax Display](#64-multi-currency-regions-and-tax-display)
  - [6.5 PriceWindow Linkage and Coverage](#65-pricewindow-linkage-and-coverage)
  - [6.6 Price Lists and Segment (Customer-Group) Pricing](#66-price-lists-and-segment-customer-group-pricing)
  - [6.7 Publish Validation, Approval, and Events](#67-publish-validation-approval-and-events)
  - [6.8 Plan Lifecycle: Versioning, Retirement, Migration](#68-plan-lifecycle-versioning-retirement-migration)
  - [6.9 Consumer Contracts (Subscriptions / Rating / Billing)](#69-consumer-contracts-subscriptions--rating--billing)
  - [6.10 Advanced Pricing Primitives](#610-advanced-pricing-primitives)
  - [6.11 Operator Efficiency](#611-operator-efficiency)
  - [6.12 Access Control and Governance](#612-access-control-and-governance)
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
  - [Plan definition and billing cycles](#plan-definition-and-billing-cycles)
  - [Plan composition, phases, and descriptors](#plan-composition-phases-and-descriptors)
  - [Pricing tiers and usage policy](#pricing-tiers-and-usage-policy)
  - [Multi-currency, tax display, and price rows](#multi-currency-tax-display-and-price-rows)
  - [Add-ons and bundles](#add-ons-and-bundles)
  - [PriceWindow linkage](#pricewindow-linkage)
  - [Publish validation, approval, and events](#publish-validation-approval-and-events)
  - [Plan lifecycle, update, and migration](#plan-lifecycle-update-and-migration)
  - [Operator efficiency](#operator-efficiency)
  - [Price preview and access control](#price-preview-and-access-control)
  - [Commercial model coverage](#commercial-model-coverage)
  - [Proration and provisioning contracts](#proration-and-provisioning-contracts)
  - [Currency and FX policy](#currency-and-fx-policy)
  - [Lifecycle safety (contract lock and entitlement overflow)](#lifecycle-safety-contract-lock-and-entitlement-overflow)
  - [Read-model and publish robustness](#read-model-and-publish-robustness)
  - [Phases and PriceList authoring](#phases-and-pricelist-authoring)
  - [Migration robustness and eligibility](#migration-robustness-and-eligibility)
  - [Cross-PRD contract conformance](#cross-prd-contract-conformance)
  - [Approval, governance, and access control](#approval-governance-and-access-control)
  - [Discounts and approval governance](#discounts-and-approval-governance)
  - [Validation completeness](#validation-completeness)
  - [Grandfathering and concurrency](#grandfathering-and-concurrency)
  - [Scope-key and tax validation](#scope-key-and-tax-validation)
  - [Lifecycle and structural integrity](#lifecycle-and-structural-integrity)
  - [Currency binding and lifecycle integrity](#currency-binding-and-lifecycle-integrity)
  - [Authoring guardrails and discount GA gate](#authoring-guardrails-and-discount-ga-gate)
  - [Package and prepaid pricing](#package-and-prepaid-pricing)
  - [Non-Functional Requirements (Show-Stoppers)](#non-functional-requirements-show-stoppers)
  - [Commercial-shape and lifecycle completeness (review follow-ups)](#commercial-shape-and-lifecycle-completeness-review-follow-ups)
- [13. Dependencies](#13-dependencies)
- [14. Assumptions](#14-assumptions)
- [15. Open Questions](#15-open-questions)
- [16. Risks](#16-risks)
- [17. Reference Materials](#17-reference-materials)
  - [17.1 Supported Billing Cycles and Price Structure Kinds (catalog)](#171-supported-billing-cycles-and-price-structure-kinds-catalog)
  - [17.2 Model Kind / Tariffs Formula Mapping (conformance)](#172-model-kind--tariffs-formula-mapping-conformance)
  - [17.3 Plan Composition Rules (normative)](#173-plan-composition-rules-normative)
  - [17.4 Price Validation Rules (catalog)](#174-price-validation-rules-catalog)
  - [17.5 Price-Change Mechanisms and CatalogVersion Increment Contract](#175-price-change-mechanisms-and-catalogversion-increment-contract)
  - [17.6 Consumer Contracts Detail](#176-consumer-contracts-detail)
  - [17.7 Advanced Pricing Primitives Detail](#177-advanced-pricing-primitives-detail)
  - [17.8 Future Scope](#178-future-scope)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

**Plan & Price Modeling** is the BSS Product Catalog capability that provides the authoritative way to define **subscription plans**, **price structures**, **add-ons**, **bundles**, and **billing descriptors** so that **Subscriptions** can sell, **Tariffs** can resolve commercial inputs, and **Rating** can charge **deterministically** and **reproducibly** from frozen snapshots.

The Catalog remains **System of Record** for `Plan`, `Price`, bundle/add-on composition rules, and billing descriptors. This capability defines **what** those primitives MUST contain so **Tariffs** can evaluate and **Rating** can charge deterministically; it does **not** compute charges, evaluate overlays, or manage subscription runtime state (§5.2).

> Where this PRD states a normative requirement, the **subject is the catalog (this PRD)** unless explicitly stated otherwise.

### 1.2 Background / Problem Statement

The Catalog is the SoR for the pricing primitives that downstream commercial systems consume. Without an explicit, publish-validated structure for plans and prices, rating inputs become ambiguous: overlapping tier bands, missing evaluation-policy fields, undeclared meters, and uncovered price windows reach production and produce non-reproducible charges, revenue leakage, and disputes.

This PRD fixes the **catalog-side structure and publish contract** — billing cycles, tier bands, model kinds, `PlanTier`, evaluation-policy fields, billing descriptors, price windows, and the consumer read-model contract — so that a published plan exposes a complete, frozen read model for Tariffs and Rating and so that no ambiguous configuration can be published (fail-closed validation). It complements **Tariffs** (evaluation semantics), **Subscriptions** (runtime, proration math), and **Billing** (posting, invoice immutability).

Industry practice for plan migrations favors **time-bounded grandfathering**, renewal-aligned cutovers, and advance notice (typically 60-90+ days for material increases). The catalog MUST support `priceEligibility` and scheduled migration paths so operators can implement these policies without cloning SKUs.

### 1.3 Goals (Business Outcomes)

- **Revenue accuracy**: zero ambiguous plan/price configuration reaching production; fail-closed publish validation.
- **Time-to-market**: self-service plan wizard, clone, and bulk price maintenance for Finance and Product teams (target >= 90% self-service for standard plan types).
- **Partner and marketplace enablement**: multi-currency, regional, and rev-share-aware bundles without SKU cloning.
- **Lifecycle safety**: retirement, grandfathering, and scheduled migration without mutating posted invoices.
- **Audit and compliance**: immutable price history, approval trails, and snapshot refs with **>= 7-year retention** (tenant/jurisdiction-configurable).
- **Engineering determinism**: published plans expose a complete read model (`{skuId, planId, priceId}`, model kind, tier bands, evaluation-policy fields, billing descriptors); plan mutations are versioned; active subscriptions continue on frozen snapshots until renewal or explicit migration; event fan-out enables cache warming within manifest SLOs.

**Success metrics**

| **Metric** | **Target** | **Measurement** |
|------------|------------|-----------------|
| Publish validation catch rate | 100% of known-invalid configs blocked before `PlanPublished` | QA + production validation audit |
| Pricing config incidents | Zero production charges from ambiguous meter or overlapping tier config | Incident tickets tagged `catalog-pricing` |
| Pre-GA config-validation coverage (leading indicator) | >= 99% of seeded-invalid configs caught in staging before GA | Staging validation-suite pass rate |
| Plan publish propagation | p95 <= 5s from `CatalogVersionPublished` (committed version) to Rating read-model visibility | Event timestamp delta |
| Plan read / preview latency | p95 <= 100ms per tenant partition | APM on catalog read APIs |
| Manual IT involvement in plan creation | >= 90% self-service for standard plan types | Ops ticket volume vs plan publish count |
| Price history completeness | 100% of published price changes retain prior row | Audit sampling |
| Migration without invoice mutation | 100% of scheduled migrations use snapshot/`PlanLink` paths only | Billing reconciliation |

### 1.4 Glossary

| **Term** | **Definition** |
|----------|----------------|
| **Plan** | Catalog entity binding a published SKU to a **billing cycle** (one-time, recurring, usage-based, hybrid), add-on rules, optional `invoiceGroupingKey`, mandatory **PlanTier**, and optional phase price schedules. |
| **Plan composition** | Commercial elements on a plan: base and usage price row(s), tier bands, add-on rules, billing descriptors — not runtime subscription state. |
| **Price row** | Catalog `Price`: amount, currency, the **canonical scope key** `(planId, currency, region, priceList, phase, priceEligibility, chargeKind)` (**no `brand` axis** — brand pricing is a brand-scoped `PriceList` overlay), `minQtyThreshold`, rounding policy reference, `taxInclusive`, `billingTiming` (**required** on recurring rows), tier bands, evaluation-policy fields (`modelKind`, `tierAggregationWindow`, `billingGranularity` — usage rows only), and optional `grandfatherUntil` / `discountRef`. |
| **Pricing tier** | Quantity band `[fromQty, toQty)` with a unit price (graduated/volume); bands MUST be non-overlapping and ordered ascending by `fromQty`. Whole-block pricing is a separate `modelKind=package`, not a tier band. An **included allowance** (N free units bundled in a recurring fee) is representable today as a `$0` first band — mathematically correct, but display/quoting cannot label it "includes N units" and reporting cannot separate included from billed consumption. A first-class `includedAllowance` (+ `rolloverPolicy`) is a decision gate (Future scope; Open Questions). |
| **Model kind** | Catalog flag: `flat`, `per_unit` (per-seat; unit price x external quantity), `graduated` (marginal per-band rates), `volume` (one rate — the band containing total quantity — applied to **all** units), or `package` (usage billed in whole blocks). Tariffs applies formulas from this flag. |
| **quantitySource** | For `per_unit` (per-seat) price rows: where the billable quantity comes from — `subscription_seat_count` (Subscriptions supplies the per-period seat count at rating time) or `manual` (the operator sets a fixed `quantity` on the price row at authoring; persisted, frozen in the snapshot, applied each period until a new plan version changes it). Catalog persists the source (and, for `manual`, the fixed quantity); it MUST NOT infer, meter, or compute a `subscription_seat_count`. |
| **Package (block) pricing** | `modelKind=package`: usage billed in whole **blocks**. Catalog persists `packageSize` (> 0 units per block) and `packagePrice` (>= 0 per block); tier-band fields MUST be absent. The round-up math (blocks = ceil(used / `packageSize`), charge = blocks x `packagePrice`) is evaluated by Tariffs. |
| **Prepaid credit grant** | Optional plan primitive for prepaid/wallet selling: `grantAmount`, `creditUnit` (a currency or a published `meteringUnit`), `price` (scoped per `(currency, region)` like a price row), `expiryPolicy` (`never` \| `days(N)`), `autoRechargeAllowed`. Catalog **describes** the grant and freezes it into `pricingSnapshotRef`; the **balance ledger, drawdown, zero cut-off, and auto-recharge execution** are owned by Billing/Rating. |
| **Tier aggregation window** | Policy for when tier counter `Q` resets: `calendar_month`, `invoice_period`, `subscription_lifetime`, or `per_event`. MUST be set on usage/tiered price rows at publish. Defines **when** `Q` resets, **not how** `Q` is derived: at launch the derivation is **sum over the window** (`aggregationFunction = sum` is the only supported value). Peak/high-water-mark, last-value, and unique-count aggregation are Future scope. |
| **Billing granularity** | Minimum billable unit for usage (`per_second`, `per_minute`, `per_hour`, `per_day`, whole-unit). MUST be set on usage price rows at publish. |
| **Derived (composite) meter** | A catalog primitive binding a **formula over two or more published metering units** (e.g. `vCPU`, `RAM`) to a **single derived metering unit** priced as one line. The catalog persists the **constituent unit ids**, the **formula expression (as data)**, and the **output unit**; the constituent base units are declared by the catalog registry (this PRD composes published units, it does not declare base meters). Tariffs evaluates the formula at rating time (math is **not** computed here); the composite definition is frozen in `pricingSnapshotRef`. |
| **Hybrid plan** | Plan combining **recurring base** price row(s) and **usage** price row(s) on the same `planId` for one SKU. |
| **Plan phase** | Named, ordered segment (e.g., trial, intro, evergreen) with its own price schedule reference and optional `convertsToPhaseId` successor. Catalog publishes the phase->price mapping, ordering, and (for `trial`) the authored `displayTrialDays` length for preview/quoting. **Trial runtime mechanics** are owned by Subscriptions, which enforces from the published `displayTrialDays`. |
| **Add-on rule** | Plan-scoped rule: eligible add-on SKU, required/optional, min/max/step quantity, optional price override reference. |
| **Bundle** | Composite of included SKUs with constraints, optional `revShare`, and `invoiceItemization` (`aggregate` \| `itemize`). For `sum_of_parts` pricing the bundle references the component **`planId`s** (not bare SKUs) so the summed rows are unambiguous per `(currency, region)` (and matching `frequency` for recurring components). |
| **PlanTier** | Manifest-required classification on SKU/Plan for Subscriptions, SLA, and quotas; distinct from **OrgTier** partner overlays. |
| **Billing descriptor set** | Per published SKU/Plan: invoice line template/labels, tax category, GL code, composition/itemization rules (manifest §4.1 contract). Plan-owned customer-facing content (plan/phase names, invoice line labels) is **single-language today**; per-locale **authoring** is owned by extending the registry's localization mechanism to Plan-owned fields, while **Presentation** owns invoice/preview rendering localization (decision — Open Questions). |
| **priceEligibility** | Who may receive a price/window: `all_subscriptions`, `new_subscriptions_only`, `existing_grandfathered`. When more than one class holds an active window on the same remaining axes, selection is **most-specific-wins**: `existing_grandfathered` > `new_subscriptions_only` > `all_subscriptions` (Tariffs step 2 applies this **after** eligibility matching), so the resolved row is always unique. The **successor** row in a grandfathering cutover carries `all_subscriptions`, so new subscriptions bind to it and a grandfathered subscription re-bound at `grandfatherUntil` expiry also resolves to it. |
| **pricingSnapshotRef** | Immutable composite reference (`CatalogVersion` + resolved price ids + evaluation-policy version) on charges and `BillableItem`. **Normative composition SoR**: Tariffs; this entry is the aligned catalog-side view and MUST NOT diverge from it. |
| **Plan migration** | Transition from retiring `planId` to target plan via Subscriptions `PlanLink` — not in-place posted invoice mutation. |
| **Grandfathering** | Carrying legacy pricing forward on a dedicated `existing_grandfathered` row for subscriptions activated before cutover (the row is immutable, so the retained price is necessarily a grandfathered **copy**); optionally **time-bound** via `grandfatherUntil` (null = indefinite). Because `priceEligibility` is part of the canonical scope key, a grandfathered row and its successor are **distinct keys** that hold active windows concurrently at the same `t` (no non-overlap violation); grandfathered subscriptions are **live-resolved** by Tariffs step 2 against an **immutable** row, so the price is stable, reconciling live resolution with the frozen-snapshot doctrine. |
| **Plan retirement** | Blocks **new** subscriptions; preserves snapshots for in-flight subscribers. |
| **Entitlement grant set** | The feature flags and quotas a Plan confers, published into the catalog read model so Subscriptions can provision and enforce them. Policy is `PlanTier`-driven (registry); enforcement is owned by Subscriptions. This PRD owns **publishing the grant set (or its `PlanTier`-resolved reference)** on the Plan, not entitlement semantics. |
| **Proration input contract** | Read-model fields each recurring price row exposes so Subscriptions can compute proration deterministically: `billingAnchorPolicy`, `prorationBasis`, and `creditOnDowngrade` (bool). Catalog publishes the inputs; proration math is owned by Subscriptions. |
| **prorationBasis** | **Canonical** apportionment convention for a partial period — **one enum owned here and adopted verbatim by Tariffs**: `calendar_days_actual`, `calendar_days_30` (fixed 30-day month, day count capped at 30), `by_second`, `whole_unit` (no sub-period proration), or `none` (no proration — full-period charge, no partial credit). MUST be authored on the plan/price policy and **frozen in `pricingSnapshotRef`**; **all** mid-period proration uses it. Catalog names the convention; Subscriptions and Tariffs compute the amount from the same frozen value. |
| **billingAnchorPolicy** | When a recurring period anchors: `calendar_month` (1st), `subscription_start` (signup day), or `fixed_day(d)`. For `fixed_day(d)` where `d` exceeds the month length the period MUST anchor on the **last day of the month**; all anchor math is **UTC**. |
| **billingTiming** | When a recurring fee is invoiced relative to its service period: `in_advance` (billed at period start) or `in_arrears` (billed at period end). **REQUIRED** on recurring price rows at publish; frozen in `pricingSnapshotRef`. Usage rows are implicitly `in_arrears`. Consumed by **Billing** (deferral policy) and Subscriptions; a hybrid plan MAY combine an `in_advance` base row with `in_arrears` usage rows. |
| **chargeKind** | Price-row axis distinguishing the charge components a plan carries at once: `recurring`, `usage`, `one_time` (a **one-time plan's base row**), or `one_time_setup` (a setup charge on a recurring/hybrid plan). Part of the canonical scope key, so a hybrid plan's components are **distinct** rows, not duplicates. |
| **one_time_setup** | A `chargeKind` for an optional one-time setup/activation charge on a recurring/hybrid plan, charged once at activation; a first-class price row (not an add-on SKU). |
| **reservedRate / reservationFlavor** | Attributes on a `usage` price row for reserved-capacity pricing: `reservedRate` is the committed unit price for the reserved/allocated quantity; `reservationFlavor` is `consumption` (matched usage at the reserved rate, remainder on-demand) or `capacity` (allocated quantity billed regardless of usage). Aligned with Tariffs `reservationMatch`. |
| **allowedChangeTargets** | Plan-change-contract field: the target `planId`s a subscription MAY move to (explicit list or a rule); absence = **no self-service change** (fail-safe). Subscriptions enforces. |
| **comparabilityRank** | Plan-change-contract integer rank classifying a change as upgrade (higher), downgrade (lower), or switch (equal); drives proration sign/credit. `PlanTier` alone is **not** an ordering unless published as authoritative. |
| **availableFrom / availableTo** | Optional plan-level **purchasability dates** for **all billing cycles** (unified one pair). Validated against window coverage at publish; a purchase still requires an active window (sellability gate). |
| **PLAL** | Pricing Logic Abstraction Layer — the Tariffs evaluation surface resolving overlays, commitment, coupons, and FX. Catalog produces inputs; PLAL evaluates. |
| **SoR** | System of Record — authoritative owner of an entity's truth (e.g. Catalog registry is SoR for Product/SKU/`CatalogVersion`). |
| **SSP** | Standalone Selling Price — ASC 606 allocation reference; out of scope here (catalog MAY carry SSP tags in Future scope). |
| **OrgTier** | A partner's commercial standing/overlay axis (channel economics); distinct from `PlanTier` (the edition/grade of a SKU). OrgTier-scoped pricing is evaluated in Tariffs. |
| **invoiceGroupingKey** | Optional Plan field that groups billable lines onto the same invoice/section; consumed by Billing for line layout. Optional (empty = no grouping). It is a **layout hint only** and MUST NOT override the single-currency-per-invoice invariant: Billing MUST split lines of different currencies onto separate invoices even when their grouping keys match. |
| **Rounding policy reference** | A named rounding-policy id referenced by a price row; policy definition and application are owned downstream (Tariffs/Billing). Catalog persists the reference only. If unset on a row, publish MUST resolve the **tenant default rounding policy**; if neither exists, publish MUST fail (no implicit rounding). |
| **dimensionKey** | Identifier for a usage dimension on a `(meter, dimensionKey)` rated line; dimensional pricing by event properties is Future scope. |
| **region** | Catalog price-scope key (a **commercial/billing territory**, e.g. `US`, `EU`), part of the canonical scope key. It is **not** a tax jurisdiction (Tax Engine maps region -> tax jurisdiction) and is **independent of the authorization region** in IdP claims: a user's authz scope governs *which* rows they MAY mutate, while pricing `region` is the row's commercial scope. Region taxonomy is tenant-configured. |
| **brand** | A tenant sub-brand/label. **Not** a price-row axis (the manifest `Price` model has no `brand` field): brand-differentiated pricing is authored as a **brand-scoped `PriceList`** overlay (`PriceList.scope=brand`; Tariffs evaluates the overlay). A brand-scoped `PriceList`'s `brand` MUST be a member of the tenant's configured **brand taxonomy** (validated at save); tenant-configured like `region`. `brand` MAY additionally be an authorization/display scope. |
| **customerGroup** | Operator-defined end-customer segment (trial/beta/VIP/on-discount/custom) for **segment pricing** via a `customerGroup`-scoped `PriceList` (adjustment per group x region, resolved by `payerTenantId`). A **BSS-owned governed taxonomy**; membership is an **effective-dated, audited BSS record** on the payer's commercial profile (not an AMS/tenant-topology attribute). The resolved group is frozen in `pricingSnapshotRef`. |
| **Plan phase types** | `trial` = time-boxed evaluation phase (runtime owned by Subscriptions); `intro` = time-boxed introductory-price phase (typically after `trial`); `evergreen` = the **terminal** steady-state phase with no `convertsToPhaseId` successor. A phased plan MUST have exactly one terminal phase. |
| **Material change** | A plan/price change whose delta is **>= the configured approval threshold** (absolute or percent, per currency; for multi-currency changes, any row exceeding its own-currency threshold). If **no** threshold is configured, **all** changes are material (fail-safe). A **first publish** has **no prior baseline**, so a delta cannot be computed — it is **always material** (fail-safe). **Two-person rule** = the **submitter plus one independent approver** (two distinct principals); the submitter MUST NOT be the approver. A change **below** an explicitly configured threshold (and not a first publish) MAY auto-publish with no additional approver. |
| **grandfatherUntil** | Optional **UTC** bound on a grandfathered price row. While null, grandfathering is **indefinite** across renewals; once passed, a bound subscription MUST re-bind to the current eligible row at its next renewal (catalog publishes the bound, Subscriptions executes). |
| **minQtyThreshold** | A floor on a price row; **distinct** from tier `fromQty`. Its semantics depend on the billing cycle and MUST be one of two **explicitly-typed** floors: **(a) purchase-quantity floor** — enforced by **Subscriptions at order time**: a purchase below the floor is **rejected** (not silently zero); **(b) usage-quantity floor** — an **eligibility** floor evaluated **downstream** (Tariffs/Rating): usage below the floor is **not eligible** for the row's pricing and MUST **fail closed** to the plan's defined fallback, **not** be silently zero-rated. The row MUST declare which floor type applies; catalog persists the type + value and freezes them in `pricingSnapshotRef`. Both MAY be set. |
| **discountRef** | Optional reference to an **external** discount instrument (owned by Promotions/Tariffs). The catalog validates **referential integrity only** — it does not author, evaluate, or stack discounts. Conditional day-1 hook. |

## 2. Architecture Alignment

| **Field** | **Value** |
|-----------|----------|
| **Applicable Manifest(s)** | BSS |
| **Relevant Chapters** | §4.1 Product and Service Catalog (Plan/Price, bundles, add-ons, billing descriptors, approvals, publish); §4.2 Rating and Charging (`pricingSnapshotRef`, stable catalog refs); §4.3 Subscriptions and Entitlements (`PlanLink`, composition); §2.1.3 Multi-tenant semantics (`PlanTier`, `{resourceTenantId, payerTenantId, sellerTenantId}`); §4.8 Marketplace (bundle rev-share where applicable) |

> **Normative alignment**: Catalog remains **SoR** for `Plan`, `Price`, bundle/add-on composition rules, and billing descriptors. This PRD defines **what** those primitives MUST contain so **Tariffs** can evaluate and **Rating** can charge deterministically. It MUST NOT contradict manifest invariants: non-overlapping `PriceWindow` rows per the **canonical scope key** (§2.2), immutable posted financials, stable `{skuId, planId, priceId}` on billable artifacts, and `pricingSnapshotRef` on outputs. **Approval interpretation**: the manifest's multi-approver workflow is read as multi-**party** — a material change requires the submitter **plus >= 1 independent approver** (two-person rule; see Glossary Material change).

### 2.1 Boundaries

> **Boundary with Tariffs**: This PRD owns catalog-side **structure and publish contract** (billing cycles, tier bands, add-on rules, `PlanTier`, model kind, evaluation-policy fields). **Tariffs** owns **evaluation semantics** (graduated vs volume math, override hierarchy, coupons, FX steps). **Rating** owns Usage -> `RatedCharge` / `BillableItem` orchestration.

> **Boundary with Price Windows**: **Scheduling**, UTC activation jobs, overlap UI, and `PriceWindowScheduled`/`Activated`/`Cancelled`/`Expired` event mechanics remain authoritative in the effective-dating price-windows use case. **This PRD** requires every billable `Price` row to be **linkable** to effective windows and **publish-validated** so Tariffs step 2 never resolves from draft-only state.

> **Boundary with Catalog registry (Product & SKU)**: `Product`, `SKU` (including the `bundle` **type** flag), `Category`, `Attribute`, the `PlanTier` **taxonomy**, the metering-unit **declaration**, and the catalog-wide **`CatalogVersion`** are owned by the Product & SKU registry. This PRD **consumes published SKUs** and **freezes** its plan/price/descriptor content into the `CatalogVersion` that the registry publishes; it MUST NOT re-author Product/SKU/Category/Attribute or re-define `CatalogVersion`. This PRD owns **Plan**, **Price**, **PriceWindow** linkage, **PriceList authoring**, **bundle composition**, **add-on rules**, and **billing descriptors**.

### 2.2 Canonical Scope Key (normative)

The single scope key for row-uniqueness, supersession, `PriceWindow` non-overlap, and window coverage is **`(planId, currency, region, priceList, phase, priceEligibility, chargeKind)`**. `chargeKind` in `recurring | usage | one_time | one_time_setup` distinguishes the price components a single plan legitimately carries at once — a hybrid plan holds a `recurring` **and** a `usage` row (optionally a `one_time_setup` row) on one `planId`, and a **one-time plan's base row is `one_time`**, so they are **distinct keys**, not duplicates.

Axis values / defaults: `priceList = base`; `phase =` a reserved implicit terminal phase (`evergreen`) for non-phased plans, one-time plans, and setup rows; `priceEligibility = all_subscriptions`; `chargeKind` per row. **Price rows authored here always carry `priceList = base`** — partner/orgTier/brand overlays are separate `PriceList` rows evaluated downstream by Tariffs; publish-time coverage resolves on the **base** key (overlays applied at evaluation). **`brand` is NOT a price-row axis**: brand-differentiated pricing is a **brand-scoped `PriceList`** overlay, consistent with the manifest §4.1 invariant. The manifest §4.1 and §8.2 non-overlap invariants and the Tariffs non-overlap key are aligned to this key (extended additively with `phase`, `priceEligibility`, `chargeKind`); the effective-dating use case's narrower `(plan, currency, region, priceList)` key is **superseded** by this canonical key for normative purposes.

### 2.3 Predecessor Documents and Scope Migration

| **Source** | **Relationship** |
|------------|------------------|
| Product Catalog & Marketplace PRD | Parent catalog PRD; Plan & Price Modeling was P0 scope with a use-case child |
| Product & SKU Management PRD | Catalog **registry** this PRD builds on (SoR for Product/SKU/Category/Attribute/`CatalogVersion`, the `bundle` SKU type, metering-unit declaration, and `PlanTier` taxonomy); upstream foundation, not superseded |
| Plan & Price Modeling use case | Legacy use-case format; **superseded** by this PRD for normative requirements. The partner **effective-price** preview (with pricing hierarchy) is **narrowed** here to base list price + overlay disclaimer; an accurate end-customer quote requires Tariffs overlay evaluation — Tariffs MUST pick up a partner-facing effective-price preview requirement (Open Questions) |
| Effective-dating price-windows use case | Authoritative for window scheduling/activation until consolidated |
| Tariffs — Commercial Pricing Logic PRD | Downstream consumer; authoritative for formula evaluation and `pricingSnapshotRef` composition |
| Metering & Pricing Module PRD | Metering collection only; pricing hierarchy evaluation in Tariffs |

## 3. Actors

### 3.1 Human Actors

#### Finance Manager

**ID**: `cpt-cf-bss-plan-price-actor-finance-manager`

**Role**: Defines plans, prices, tiers, tax-inclusive flags, and submits publishes for approval.
**Needs**: Accurate recurring/usage pricing; audit trail; safe price changes.

#### Product Manager

**ID**: `cpt-cf-bss-plan-price-actor-product-manager`

**Role**: Configures add-ons, bundles, and plan composition.
**Needs**: Package deals; compatibility rules; marketplace bundles.

#### Finance Reviewer

**ID**: `cpt-cf-bss-plan-price-actor-finance-reviewer`

**Role**: Approves material price/plan publishes under the two-person rule.
**Needs**: Two-person rule; rejection with reason; threshold controls.

#### Partner / System Integrator

**ID**: `cpt-cf-bss-plan-price-actor-partner`

**Role**: Previews list prices for end-customers.
**Needs**: Region/currency-scoped catalog base price before Tariffs overlays (requires an explicit catalog-preview read grant).

#### Catalog Admin

**ID**: `cpt-cf-bss-plan-price-actor-catalog-admin`

**Role**: Tenant-scoped catalog governance.
**Needs**: RBAC, bulk operations, retirement and migration orchestration; the scoped historical-import (backdating) capability where authorized.

#### Auditor

**ID**: `cpt-cf-bss-plan-price-actor-auditor`

**Role**: Reads price history and approval records.
**Needs**: Immutable history; export; no PII in events beyond policy.

### 3.2 System Actors

#### Catalog Registry (Product & SKU)

**ID**: `cpt-cf-bss-plan-price-actor-catalog-registry`

**Role**: SoR for Product/SKU/Category/Attribute/`CatalogVersion`, the `bundle` SKU type, metering-unit declaration, and `PlanTier` taxonomy; the **sole** incrementer of `CatalogVersion`.

#### Tariffs / PLAL

**ID**: `cpt-cf-bss-plan-price-actor-tariffs`

**Role**: Consumes the published read model (model kind, tiers, evaluation-policy fields, derived-meter definition, reserved rate, `customerGroup` overlays) and evaluates commercial prices; composes `pricingSnapshotRef`.

#### Rating

**ID**: `cpt-cf-bss-plan-price-actor-rating`

**Role**: Consumes events and warmed read models; resolves `{skuId, planId, priceId}` and evaluation-policy fields deterministically; never reads draft state.

#### Subscriptions

**ID**: `cpt-cf-bss-plan-price-actor-subscriptions`

**Role**: Sells eligible plans; executes proration math, plan-change classification, trial runtime, entitlement enforcement, `PlanLink` migration, and sellability checks from the published inputs.

#### Billing

**ID**: `cpt-cf-bss-plan-price-actor-billing`

**Role**: Consumes billing descriptors via `CatalogVersion`; derives deferral policy from `billingTiming`; posts immutable invoices.

#### Marketplace

**ID**: `cpt-cf-bss-plan-price-actor-marketplace`

**Role**: Consumes bundle rev-share rules for fee accrual.

#### Tax Engine

**ID**: `cpt-cf-bss-plan-price-actor-tax-engine`

**Role**: Determines tax scheme (VAT/GST/US sales tax) and maps `region` -> tax jurisdiction; consumes `taxInclusive` display basis and `taxCategory` reference (no scheme/calc here). Post-MVP dependency.

#### Promotions

**ID**: `cpt-cf-bss-plan-price-actor-promotions`

**Role**: Owns coupon/discount authoring and evaluation; provides the external discount instrument that `discountRef` resolves against.

#### Contracts

**ID**: `cpt-cf-bss-plan-price-actor-contracts`

**Role**: Supplies contract locks and negotiated RI-style reservation rates (per-account overlay); contract-locked subscriptions are excluded from scheduled migration.

## 4. Operational Concept & Environment

### 4.1 Module-Specific Environment Constraints

- **Multi-tenant isolation**: plans, prices, and price lists are tenant-scoped; brand/region are scoped per IdP claims at the gateway. Pricing `region` is decoupled from the authorization region claim.
- **Time**: all effective dating, window boundaries, `grandfatherUntil`, `availableFrom`/`availableTo`, and anchor math are **UTC** (per manifest).
- **Determinism / snapshot**: published plans expose a complete, frozen read model; consumers resolve via `pricingSnapshotRef` and MUST NOT read draft state or substitute defaults for absent evaluation-policy fields. Posted invoice periods MUST NOT re-query mutable catalog rows.
- **No charge computation**: the catalog persists and publishes structure; it MUST NOT compute monetary charges, evaluate overlays, or perform FX. Mathematical formulas belong in Tariffs.
- **Precision**: amount precision follows the **ISO 4217 minor unit** for the row's currency (0 for JPY/KRW, 2 default, 3 for BHD/KWD/OMR); a flat 2-decimal cap MUST NOT be assumed.
- **Region / brand taxonomy**: `region` and brand-scoped `PriceList` `brand` values MUST be members of the tenant's configured taxonomies, validated before publish.

**Event alignment (manifest §4.1)**

- **Producers (frozen event-name contract — names are normative; no aliases or renames without a versioned contract change)**: `PlanCreated`, `PlanUpdated`, `PlanPublished`, `PlanRetired`, `PlanMigrationScheduled`, `PlanPublishDegraded`, `BundleUpdated`, `PriceCreated`, `PriceUpdated`. Conditional emission governs *when* an event fires, not *whether* the name set is stable.
- **Manifest §4.1 alignment (additive)**: manifest §4.1 lists these producers additively alongside the pre-existing `PlanCreated/Updated`, `BundleUpdated`, `CatalogVersionPublished`, and `PriceWindowScheduled/Activated/Expired`. `CatalogVersionPublished` (registry) and `PriceWindow*` (effective-dating use case) are **consumed** here, not re-declared.
- **Consumers (downstream)**: Rating (read-model warm), Subscriptions (eligibility + `PlanMigrationScheduled` -> `PlanLink` creation), Tariffs (policy warm), Billing (descriptor cache), Marketplace (bundle/listing).

## 5. Scope

### 5.1 In Scope

> Priority mapping: HIGH -> `p1`, MEDIUM -> `p2`, LOW -> `p3`. Detailed requirements are carried as functional requirements (§6), acceptance criteria (§12), and normative appendices (§17).

| **Feature** | **Priority** | **Notes** |
|-------------|--------------|-----------|
| Plan definition: one-time, recurring (monthly/quarterly/semiannual/annual/custom), usage-based | `p1` | Draft -> published; parent SKU published |
| Hybrid plans (recurring base + usage) | `p1` | Same `planId`; publish validation |
| Per-seat (per-unit recurring) plans | `p1` | `modelKind=per_unit`; `quantitySource=subscription_seat_count\|manual`; quantity resolved by Subscriptions |
| Custom billing frequency | `p2` | `quarterly`, `semiannual`, `customEveryN{Days\|Months}(n)`; interval `n` validated (> 0) |
| Plan composition and mandatory `PlanTier` | `p1` | Manifest §4.1; `PlanTier` taxonomy + SKU-level value owned by registry; enforced at plan publish here |
| Plan phase price schedules (catalog read model) | `p1` | trial/intro/evergreen price mapping + ordering + `convertsToPhaseId`; Subscriptions owns trial runtime |
| Proration input contract publication | `p1` | `billingAnchorPolicy`, `prorationBasis`, `creditOnDowngrade` on recurring rows; Subscriptions computes proration |
| Billing timing (`billingTiming`) on recurring rows | `p1` | `in_advance \| in_arrears`, required at publish, frozen; Billing deferral policy |
| Plan-change contract publication | `p1` | `allowedChangeTargets` + `comparabilityRank`; Subscriptions classifies upgrade/downgrade/switch |
| Plan sellability gate + `availableFrom`/`availableTo` (all cycles) | `p1` | Purchase blocked without an active window + committed version; unified purchasability dating; joint with Subscriptions |
| Optional one-time setup/activation charge | `p2` | First-class one-time row on recurring/hybrid plans, charged once at activation |
| Entitlement grant set in published read model | `p1` | Feature flags/quotas (or `PlanTier`-resolved ref); Subscriptions provisions/enforces |
| Base and regional price rows (multi-currency) | `p1` | ISO 4217; `region` scope; `brand` via brand-scoped `PriceList` overlay (not a price-row axis) |
| Price list authoring (`PriceList`) | `p1` | Author/validate scope (partner/orgTier/brand/region/**customerGroup**/global), adjustment (markup/discount/fixed), explicit precedence. Precedence/stacking **evaluation** remains in Tariffs |
| Customer-group segment pricing (`customerGroup` `PriceList` scope) | `p1` | Adjustment per group x region resolved via `payerTenantId`; BSS-owned group taxonomy + effective-dated audited membership; resolved group frozen in snapshot; membership change = material change. Different per-group tier *structures* stay Future |
| Usage pricing tiers with explicit model kind | `p1` | graduated vs volume; non-overlapping bands |
| Package (block) pricing (`modelKind=package`) | `p2` | `packageSize`/`packagePrice`; round-up math in Tariffs |
| Prepaid credit grant (catalog definition) | `p2` | Grant fields frozen in snapshot; balance/drawdown owned by Billing/Rating (External dependency, GA gate) |
| Reserved-capacity price (self-service reserved rate + on-demand) | `p1` | `reservedRate`/`reservationFlavor` on the usage row; Tariffs evaluates (step 6) sourcing the rate from the snapshot |
| Interim `discountRef` day-1 discount hook | `p2` | Referential-integrity only; external discount instrument; evaluation in Tariffs/Promotions |
| `tierAggregationWindow` and `billingGranularity` on usage prices | `p1` | Required at publish; consumed by Tariffs |
| Tax-inclusive / tax-exclusive price flag per row | `p1` | Display and downstream tax basis; no tax calc here |
| Billing descriptor set per Plan/SKU | `p1` | Invoice template, tax category, GL, itemization |
| Add-on rules (dependency, bounds, overrides) | `p1` | Compatibility validation |
| Bundle definition (SKUs, constraints, rev-share, itemization, price basis) | `p2` | Marketplace; `sum_of_parts` vs `own_price` |
| `PriceWindow` linkage and publish-time coverage check | `p1` | Scheduling in effective-dating use case; linkage + validation here |
| `priceEligibility` / grandfathering on price rows (incl. `grandfatherUntil` bound) | `p1` | Tariffs step 2 consumes; renewal re-bind via Subscriptions |
| Catalog publish contract (events + read model) | `p1` | `PlanCreated/Updated/Published/Retired` |
| Consumer read-model resolution contract | `p1` | Tariffs/Rating deterministically resolve published inputs; no draft read, no default substitution |
| Publish fan-out atomicity / degraded handling | `p1` | Retry-to-SLO or `PlanPublishDegraded`; no charge against partially-published plan |
| Plan publish validation (fail-closed) | `p1` | Meters, tiers, descriptors, hybrid, PlanTier |
| Plan approval workflow (two-person rule) | `p1` | Thresholds; reject with reason |
| Tenant policy objects (approval threshold, tax-display) | `p1` | Fail-safe defaults: two-person rule if no threshold; fail-closed tax-display |
| Plan retirement and subscriber migration | `p1` | Grandfathering, scheduled cutover, alt plan |
| Migration idempotency & cancellation | `p2` | Re-trigger-safe; cancel-before-effective without affecting already-migrated subs |
| Legacy subscription snapshot synthesis | `p2` | Synthesize+freeze `pricingSnapshotRef` for subs lacking one (`migrated-origin`) |
| Plan versioning and immutable price history | `p1` | New price row on change; audit; see Price-change mechanisms (§17.5) |
| Contract-locked plan protection | `p1` | Reject structural mutation |
| Effective catalog price preview | `p2` | Base list price; Tariffs for full hierarchy. Draft-plan bill simulation needs a draft/sandbox extension (Open Questions) |
| Plan clone / duplicate | `p2` | Draft copy with new ids |
| Bulk price import/update | `p2` | All-or-nothing **validation**; **per-row commit** with conflict report |
| Mass repricing (idempotent, deduplicated events) | `p2` | Re-run-safe bulk adjustment with throughput SLO |
| Mutation idempotency keys | `p2` | Plan/Price create/update accept client idempotency key; duplicate returns original |
| Price history view and export | `p2` | Auditor/Finance |
| One-time availability and quantity constraints | `p2` | `availableFrom`/`availableTo`, min/max qty |
| Operator UX (wizard, tier editor, migration UI) | `p2` | Frontend DESIGN |
| RBAC: CatalogAdmin, FinanceReviewer, ProductManager, Auditor | `p1` | Tenant/brand/region scoped |
| Data residency for price/audit data per jurisdiction | `p2` | Aligns with jurisdiction-configurable audit retention NFR |

### 5.2 Out of Scope

- **Tariff evaluation formulas**, override hierarchy, coupon stacking, FX math — Tariffs. This includes **evaluating the derived (composite) meter formula**: this PRD persists the composite definition (units, formula-as-data, output unit); Tariffs computes the result.
- **`PriceWindow` scheduler UI**, UTC activation jobs, cutoff cron — the effective-dating price-windows use case.
- **`PriceList` precedence/stacking evaluation and contract override evaluation** — Tariffs + Contracts. (`PriceList` **authoring/validation** is in Scope; see §5.1.)
- **Subscription state machine, proration math, trial runtime mechanics, entitlement enforcement, plan-change-mid-cycle, downgrade entitlement-overflow enforcement, `PlanLink` execution** — Subscriptions. This PRD publishes the **inputs**; Subscriptions executes.
- **Cross-currency / cross-region / cross-frequency mid-cycle plan changes** — NOT supported at launch (Future scope); handled as cancel + new subscription.
- **Coupon / discount / promo-code authoring** and coupon **evaluation/stacking** — Promotions domain / Tariffs. Neither coupon authoring nor evaluation is a catalog concern here.
- **Refunds, credit/debit notes, refund/credit eligibility execution** — Billing / Payments. Catalog does not flag per-row refundability (Future).
- **Prepaid credit balance ledger, drawdown, zero cut-off, auto-recharge execution** — Billing / Rating. Catalog only **defines** the grant (frozen in snapshot); balance is never tracked here.
- **PSP card rails, payment gateway integration, ERP/GL export** — Payments / Billing. Catalog supplies `glCode`/descriptors via `CatalogVersion` only.
- **Usage ingestion, `UsageRecord` shapes** — OSS / Metering.
- **Tax-scheme determination (VAT/GST/US sales tax), tax calculation, invoice posting, GL posting** — Tax Engine / Billing. Catalog owns only the tax **display basis** (`taxInclusive`) and `taxCategory` reference.
- **API schemas, proto, error codes** — Design doc(s).
- **Full ASC 606 recognition** — Finance/Billing; catalog MAY carry SSP reference tags in Future scope.
- **Self-service term / auto-renew metadata** (`termLength`/`autoRenew` on the Plan) — NOT a catalog concern at MVP; Billing's Catalog-term default is deferred with it (Future scope).

## 6. Functional Requirements

> **Content boundary**: FRs define WHAT the catalog MUST persist, validate, and publish, not data models or APIs. Concrete schemas, proto definitions, error taxonomies, and mathematical formulas are owned by the corresponding DESIGN. Heavy normative reference tables (billing-cycle requirements, price-structure kinds, model-kind conformance, plan-composition rules, price-validation rules, price-change mechanisms, consumer contracts, and advanced primitives) are preserved verbatim in §17.

### 6.1 Plan Definition and Billing Cycles

#### Supported billing cycles

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-billing-cycles`

The catalog **MUST** support one-time, recurring, usage-based, and hybrid billing cycles with the per-cycle catalog requirements in §17.1. A Plan **MUST** reference a **published** SKU and be storable in `draft` before publish. Recurring frequency **MUST** support `monthly`, `quarterly`, `semiannual`, `annual`, and `customEveryN{Days|Months}(n)`.

**Rationale**: The full commercial-cycle matrix is the foundation for IaaS/PaaS/SaaS and marketplace selling.

**Actors**: `cpt-cf-bss-plan-price-actor-finance-manager`

#### Custom billing frequency

- [ ] `p2` - **ID**: `cpt-cf-bss-plan-price-fr-custom-frequency`

For a recurring Plan the catalog **MUST** persist the interval as metadata for `quarterly`, `semiannual`, or `customEveryN{Days|Months}(n)` and **MUST** reject a non-positive `n`. A `customEveryN Days(n)` cycle **MUST** anchor on `subscription_start` (a `calendar_month`/`fixed_day` anchor **MUST** fail publish), and `n` **MUST NOT** exceed the configured custom-interval cap.

**Rationale**: Custom cycles must be explicit and anchor-compatible to rate deterministically.

**Actors**: `cpt-cf-bss-plan-price-actor-finance-manager`

#### Hybrid plan completeness

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-hybrid-completeness`

A hybrid Plan **MUST** declare both a recurring base price row and a usage price row on the same `planId`; publish validation **MUST** reject a hybrid missing either part, and the read model **MUST** expose both rows under one `planId` for Tariffs hybrid evaluation.

**Rationale**: Hybrid economics require both components present and co-resolvable under one plan identity.

**Actors**: `cpt-cf-bss-plan-price-actor-tariffs`

#### Per-seat (per-unit recurring) pricing

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-per-seat`

A `modelKind=per_unit` row **MUST** persist a unit price and a `quantitySource` (`subscription_seat_count` | `manual`; and, for `manual`, the fixed quantity). The catalog **MUST NOT** infer, meter, or compute the quantity (zero metering-unit footprint); the read model **MUST** let Rating/Tariffs resolve the per-period quantity from the declared source.

**Rationale**: Per-seat pricing must be explicit about quantity provenance without metering it.

**Actors**: `cpt-cf-bss-plan-price-actor-subscriptions`

#### Optional one-time setup/activation charge

- [ ] `p2` - **ID**: `cpt-cf-bss-plan-price-fr-one-time-setup`

Recurring and hybrid plans **MAY** declare an optional one-time setup/activation price row (`chargeKind=one_time_setup`) on the same `planId`, charged **once at activation** and frozen in `pricingSnapshotRef`. It **MUST** be a first-class plan price row (participating in approvals, snapshot, preview) — **not** a synthetic add-on SKU — and publish **MUST** validate it as one-time (no recurrence, no `billingTiming`/tier fields).

**Rationale**: Setup fees are common and must be first-class, not modeled as fake add-ons.

**Actors**: `cpt-cf-bss-plan-price-actor-finance-manager`

### 6.2 Price Structure and Model Kinds

#### Explicit model kind

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-model-kind`

Each price row **MUST** persist an explicit `modelKind` (`flat` | `per_unit` | `graduated` | `volume` | `package`) with no implicit default at rating time; a tiered row with unspecified model kind **MUST NOT** publish. The catalog **MUST NOT** compute charges — Tariffs applies the formula per the conformance mapping (§17.2).

**Rationale**: Rating divergence is prevented only when the model kind is explicit and frozen.

**Actors**: `cpt-cf-bss-plan-price-actor-tariffs`

#### Tier band validation

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-tier-validation`

Under the `[fromQty, toQty)` convention, tier bands **MUST** be ascending by `fromQty`, non-overlapping, and contiguous (no gaps); the top band **MUST** be open-ended (`toQty=null`) unless an explicit maximum is intended. A **closed** top band **MUST** trigger a publish-time warning, and quantity exceeding it **MUST** fail closed at evaluation (no free, no clamp); the read model persists the explicit maximum and the fail-closed marker.

**Rationale**: Off-by-one, overlap, and gap errors at band edges cause silent mispricing.

**Actors**: `cpt-cf-bss-plan-price-actor-tariffs`

#### Package (block) pricing

- [ ] `p2` - **ID**: `cpt-cf-bss-plan-price-fr-package-pricing`

A `modelKind=package` row **MUST** persist `packageSize` (> 0) and `packagePrice` (>= 0) with tier-band fields **absent**; publish **MUST** reject otherwise. The read model **MUST** expose `packageSize`/`packagePrice`; Tariffs computes the round-up math (§17.2) and the catalog **MUST NOT** compute the charge.

**Rationale**: Block pricing is a distinct model that must be authorable and gated on a joint fixture.

**Actors**: `cpt-cf-bss-plan-price-actor-tariffs`

#### Model kind / Tariffs formula conformance

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-model-kind-conformance`

The catalog `modelKind` enum and the Tariffs formula matrix **MUST** reconcile one-to-one per §17.2; publish of any `modelKind` lacking a **joint golden conformance fixture** **MUST** be blocked. `package` (repeating-block) and `per_unit` (external-quantity) **MUST** each have a joint golden fixture before publish.

**Rationale**: A single kind-to-formula source of truth prevents catalog/Tariffs divergence.

**Actors**: `cpt-cf-bss-plan-price-actor-tariffs`

#### Price amount and currency validation

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-price-amount-validation`

A price amount **MUST** be >= 0 (`0` valid for free tiers, `trial`/`intro` phases, and the first graduated band; negatives rejected), currency **MUST** be valid ISO 4217, and precision **MUST** follow the currency's ISO 4217 minor unit. The catalog **MUST NOT** perform FX: if no row exists for a requested `(currency, region)`, preview/publish **MUST** fail closed (no base-currency fallback unless an explicit `currencyFallbackPolicy` is configured — Future).

**Rationale**: Financial correctness requires non-negative, correctly-scaled, FX-free catalog amounts.

**Actors**: `cpt-cf-bss-plan-price-actor-finance-manager`

### 6.3 Plan Composition, Descriptors, and Phases

#### Mandatory PlanTier

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-plantier-mandatory`

Every Plan **MUST** declare `PlanTier` before publish (optional at draft); the taxonomy and SKU-level value are owned by the registry. Publish **MUST** validate that the Plan's `PlanTier` **equals its parent SKU's `PlanTier`** unless an explicit, audited override is declared (no silent divergence), so downstream reads one unambiguous effective tier.

**Rationale**: Subscriptions/SLA/quotas depend on one unambiguous, present tier.

**Actors**: `cpt-cf-bss-plan-price-actor-subscriptions`

#### Meter injectivity

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-meter-injective`

Each usage plan **revision** **MUST** map exactly one `meteringUnit` (per-row injectivity: one priced line per `(meter, dimensionKey)`); ambiguous mapping **MUST** fail publish. Multi-meter offerings **MUST** be modeled either as a **derived (composite) meter** (one output unit) or as separate single-meter SKUs composed via bundle/add-ons.

**Rationale**: Injective meter mapping prevents line collisions and ambiguous rating.

**Actors**: `cpt-cf-bss-plan-price-actor-rating`

#### Add-on rules

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-addon-rules`

Add-on SKUs **MUST** be published and compatible with the base SKU; conflicting add-on pairs and dependency **cycles** **MUST** fail publish. A required add-on **MUST** have `maxQty >= 1`, and an optional price override reference **MUST** be persisted on the plan snapshot.

**Rationale**: Constrained, acyclic add-on composition keeps subscription assembly valid.

**Actors**: `cpt-cf-bss-plan-price-actor-product-manager`

#### Bundle composition

- [ ] `p2` - **ID**: `cpt-cf-bss-plan-price-fr-bundle-composition`

A Bundle **MUST** declare its price basis (`sum_of_parts` or `own_price`), reference **published** `includedSkuIds`, and (for `sum_of_parts`) reference component **`planId`s** whose rows cover **every** `(currency, region)` the bundle sells in (matching `frequency` for recurring components) — a missing/ambiguous component row **MUST** fail publish. Rev-share **MUST** sum to 100% per included vendor SKU with an explicit platform cut and a nominated primary party absorbing rounding residual within <= 0.01% tolerance; `invoiceItemization` (`aggregate`|`itemize`) **MUST** be persisted and preserve per-SKU rev-share for Marketplace accrual.

**Rationale**: Marketplace bundles must be unambiguous, currency-covered, and rev-share-reconciled.

**Actors**: `cpt-cf-bss-plan-price-actor-marketplace`

#### Billing descriptor completeness

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-billing-descriptors`

Publish **MUST NOT** proceed without the complete billing descriptor set per manifest §4.1 (invoice line template, tax category, GL code, composition/itemization rules); the validation report **MUST** list any missing descriptor fields. The set **MUST** be sufficient for Billing/ERP to post without re-querying mutable catalog rows.

**Rationale**: Invoice reproducibility requires a complete, frozen descriptor set at publish.

**Actors**: `cpt-cf-bss-plan-price-actor-billing`

#### Plan phase price schedule

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-plan-phases`

For a phased plan the read model **MUST** map each phase id to its price row references, persist phase ordering and each phase's `convertsToPhaseId` successor, reject a dangling or cyclic `convertsToPhaseId`, and require **exactly one terminal phase**. For a `trial` phase the catalog **MUST** publish `displayTrialDays` for preview/quoting; Subscriptions enforces trial runtime from this same value (single source).

**Rationale**: Phase->price resolution and trial length must be catalog-authored and acyclic; runtime stays in Subscriptions.

**Actors**: `cpt-cf-bss-plan-price-actor-subscriptions`

### 6.4 Multi-Currency, Regions, and Tax Display

#### Multi-currency price rows

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-multi-currency-rows`

A Plan **MUST** support independent price rows per `(currency, region)` linked to the same `planId`, emitting `PriceCreated` per row. The system **MUST** support **at least 20 currencies per plan** as a guaranteed floor and hold the read SLO at that floor.

**Rationale**: Global selling requires many first-class per-market rows without FX derivation.

**Actors**: `cpt-cf-bss-plan-price-actor-finance-manager`

#### Region and brand taxonomy validation

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-region-brand-taxonomy`

A price row's `region` **MUST** be a member of the tenant's configured region taxonomy; an unknown region **MUST** fail validation before publish. `brand` is **not** a price-row field: a **brand-scoped `PriceList`**'s `brand` **MUST** be a member of the tenant's configured brand taxonomy or fail validation.

**Rationale**: Uncontrolled region/brand values corrupt scope resolution and overlay matching.

**Actors**: `cpt-cf-bss-plan-price-actor-catalog-admin`

#### Tax display basis

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-tax-display-basis`

The catalog **MUST** persist the `taxInclusive` display basis and `taxCategory` reference only (no scheme/calc). A `taxInclusive=true` row without a region tax rate, and a `taxInclusive=false` row in a region with no configured `taxCategory`, **MUST** be governed by the tenant tax-display policy (default **fail-closed**). Because Tax Engine is post-MVP, a `taxInclusive=true` plan **MAY** be authored but **MUST** be flagged **not-sellable-GA** until Tax Engine GA; MVP sells tax-exclusive.

**Rationale**: Display basis is a catalog concern; tax scheme/calc and jurisdiction mapping are gated on Tax Engine.

**Actors**: `cpt-cf-bss-plan-price-actor-tax-engine`

#### Single-currency-per-invoice binding

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-invoice-currency-binding`

Publish/preview **MUST** reject the enumerated configurations that would force mixed-currency lines onto a single invoice: (i) an add-on/override/required-add-on row lacking a row in a currency the base plan publishes; (ii) a `sum_of_parts` bundle whose component rows do not cover every currency the bundle sells; (iii) an `own_price` bundle without a matching-currency component set. Currency selection at activation is owned by Subscriptions; the single-currency-per-invoice invariant is enforced here.

**Rationale**: A subscription must resolve all lines in one bound currency; mixed-currency invoices are invalid.

**Actors**: `cpt-cf-bss-plan-price-actor-billing`

### 6.5 PriceWindow Linkage and Coverage

#### Publish-time window coverage

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-pricewindow-coverage`

For billable usage at time `t`, an active or scheduled `PriceWindow` **MUST** exist for the resolved **canonical scope key** (resolved on the **base** `priceList`) or publish **MUST** fail (no silent fallback), directing the operator to schedule a window. Because `priceEligibility` and `chargeKind` are part of the key, a grandfathered row and its successor, and the components of a hybrid plan, are **distinct keys** that MAY each hold an active window at the same `t` — not an overlap violation.

**Rationale**: Publishing a billable row without window coverage lets Tariffs step 2 resolve nothing (fails closed).

**Actors**: `cpt-cf-bss-plan-price-actor-tariffs`

#### Future-gap coverage across scheduled windows

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-future-gap-coverage`

For two or more scheduled `PriceWindow` rows on one scope key, publish validation **MUST** reject any uncovered time interval (gap) between the end of one active/scheduled window and the start of the next for billable periods, directing the operator to close the gap.

**Rationale**: A coverage gap fails rating closed for everyone during the gap.

**Actors**: `cpt-cf-bss-plan-price-actor-finance-manager`

#### Sellability gate

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-sellability-gate`

A subscription/purchase **MUST NOT** be created while **either** no active (not merely scheduled) `PriceWindow` covers the bound canonical scope key at `t`, **or** the plan is not yet addressable in a **committed `CatalogVersion`** (joint rule with Subscriptions). Optional plan-level `availableFrom`/`availableTo` (the unified pair for **all** billing cycles), when set, **MUST** be validated against window coverage at publish; deferred publish ("publish at T") is out of launch scope.

**Rationale**: Selling before a plan is both windowed and rateable produces a first rating that resolves no window.

**Actors**: `cpt-cf-bss-plan-price-actor-subscriptions`

#### Grandfathering eligibility

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-grandfathering-eligibility`

The read model **MUST** expose `priceEligibility` (`all_subscriptions` | `new_subscriptions_only` | `existing_grandfathered`) and any `grandfatherUntil` (UTC) for Tariffs step 2; new subscriptions after cutover **MUST NOT** bind to a grandfathered row, and a bound subscription renewing on/after `grandfatherUntil` **MUST** be signalled no longer eligible so Subscriptions re-binds it to the current eligible row (null = indefinite).

**Rationale**: Grandfathering and new-only eligibility are first-class commercial rules resolved deterministically.

**Actors**: `cpt-cf-bss-plan-price-actor-subscriptions`

### 6.6 Price Lists and Segment (Customer-Group) Pricing

#### PriceList authoring and precedence

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-pricelist-authoring`

The catalog **MUST** author and validate `PriceList` rows: scope (partner/orgTier/brand/region/customerGroup/global), adjustment type (markup/discount/fixed), and an explicit `precedence`; it **MUST** reject a duplicate `precedence` within one scope class. A `PriceList` adjustment **MAY** be effective-dated via its own `[effectiveFrom, effectiveTo)` interval (validated per scope + adjustment target, not on the canonical price-row key), and **MUST** declare its tax basis (or explicitly delegate to Tariffs) or fail publish. Precedence/stacking **evaluation** remains in Tariffs.

**Rationale**: Deterministic overlay ordering and unambiguous tax basis require authored, validated precedence.

**Actors**: `cpt-cf-bss-plan-price-actor-catalog-admin`

#### PriceList referential integrity

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-pricelist-referential-integrity`

A `PriceList` row whose scope references a plan or SKU that is not published **MUST** be rejected with a clear business error and **MUST NOT** be exposed in the read model.

**Rationale**: Overlays referencing draft/unpublished targets would resolve against non-existent base rows.

**Actors**: `cpt-cf-bss-plan-price-actor-catalog-admin`

#### Customer-group segment pricing

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-customer-group-pricing`

The catalog **MUST** author a `customerGroup`-scoped `PriceList` (adjustment per group x region), own the **group taxonomy** (BSS-governed, validated at authoring), and publish an **effective-dated, audited membership** record on the payer's commercial profile (resolved via `payerTenantId`) into the read model; the **resolved group** **MUST** be frozen in `pricingSnapshotRef`. A membership change is renewal-aligned by default (immediate re-resolution is an explicit material change); a group discount/move affecting many payers is a **material change**, and all membership changes **MUST** be audited. Entirely different **tier structures** per group are out of launch scope. Overlay evaluation is owned by Tariffs.

**Rationale**: Segment pricing must be governed, audited, and deterministically frozen without changing tenant topology.

**Actors**: `cpt-cf-bss-plan-price-actor-tariffs`

### 6.7 Publish Validation, Approval, and Events

#### Fail-closed publish validation

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-publish-validation-failclosed`

Publish **MUST** be blocked (and `PlanPublished` **MUST NOT** be emitted, nor Rating read models warmed) when validation detects any invalid condition in the aggregate fail-closed set (§17.4 price-validation rules and §12 AC #26), including ambiguous meter, overlapping/gapped tiers, unspecified `modelKind` on a tiered row, evaluation-policy fields on a non-usage row, invalid `package`/prepaid config, missing `PlanTier`/descriptors, invalid hybrid, currency precision over the ISO 4217 minor unit, unknown region/brand, add-on dependency cycle, uncovered window gap, a recurring row missing `billingTiming`, a non-one-time setup row, or a `minQtyThreshold` with no declared floor type.

**Rationale**: A single aggregate fail-closed gate prevents any ambiguous configuration from reaching production.

**Actors**: `cpt-cf-bss-plan-price-actor-finance-manager`

#### Two-person rule and segregation of duties

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-approval-two-person`

A **material change** (above the configured threshold, or a first publish with no baseline) **MUST** require **one independent approver** before `PlanPublished` (submitter + 1 approver = two distinct principals); the submitter **MUST NOT** satisfy the approval slot, and any self-approval attempt **MUST** be rejected and audit-logged. Submitter and approver identities and timestamps **MUST** be logged; a rejection returns the Plan to `draft` with reason and notifies the submitter.

**Rationale**: Unauthorized or self-approved pricing change is a financial-fraud risk.

**Actors**: `cpt-cf-bss-plan-price-actor-finance-reviewer`

#### Approval-threshold policy

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-approval-threshold-policy`

The tenant approval-threshold policy **MUST** express materiality as an absolute amount or percentage delta **per currency**; for a multi-currency change each affected row's delta is compared in its **own** currency and the rule trips if **any** row exceeds its threshold. The system **MUST** apply the two-person rule by default and **MAY** auto-publish with no independent approver **only if** a threshold is explicitly configured **and** the change is below it **and** it is not a first publish.

**Rationale**: Fail-safe materiality (two-person rule unless explicitly below an configured threshold) prevents silent large changes.

**Actors**: `cpt-cf-bss-plan-price-actor-finance-reviewer`

#### Publish fan-out atomicity and degraded handling

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-publish-fanout-atomicity`

Once the version commits (`CatalogVersionPublished`), the system **MUST** either complete read-model warming within the 5s SLO via retry or mark the publish degraded and emit `PlanPublishDegraded`; it **MUST NOT** leave a state where charges can resolve against a partially-published plan. The pre-commit batching delay (`PlanPublished` -> `CatalogVersionPublished`) is governed by the max batching-delay SLO, not by `PlanPublishDegraded`.

**Rationale**: Partial publish must never expose a rateable-but-incomplete plan.

**Actors**: `cpt-cf-bss-plan-price-actor-rating`

#### Frozen event-name contract

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-event-contract`

The catalog **MUST** emit `PlanCreated`, `PlanUpdated`, `PlanPublished`, `PlanRetired` (and, conditionally, `PlanMigrationScheduled`, `PlanPublishDegraded`, `BundleUpdated`, `PriceCreated`, `PriceUpdated`) with the frozen names in §4 (no aliases or renames without a versioned contract change), ordered per `(tenantId, aggregateId)` where the manifest applies, carrying correlation/idempotency keys so consumers can dedupe (at-least-once delivery).

**Rationale**: Downstream cache warming and eligibility depend on a stable, ordered, dedupable event contract.

**Actors**: `cpt-cf-bss-plan-price-actor-rating`

#### CatalogVersion increment on publish

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-catalogversion-increment`

On every `PlanPublished` the plan's content **MUST** become addressable in a `CatalogVersion` with the registry as the **sole** incrementer; the registry **MAY batch** multiple approved publishes into one discretionary catalog publish (not one dedicated version per `PlanPublished`). `PlanPublished` carries a **pending** version reference; the committed `CatalogVersion` is finalized when the registry emits `CatalogVersionPublished`, and `pricingSnapshotRef` **MUST** pin that committed version (§17.5).

**Rationale**: Determinism requires pinning the exact committed version the registry batches the plan into.

**Actors**: `cpt-cf-bss-plan-price-actor-catalog-registry`

#### Consumer read-model resolution and monotonicity

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-consumer-readmodel-resolution`

Consumers **MUST** resolve `{skuId, planId, priceId}`, model kind, ordered tier bands, and evaluation-policy fields exactly as published, from the committed `CatalogVersion`, **without** reading draft state and **without** substituting any default for an absent evaluation-policy field (absence must have failed publish). The read model **MUST** be **monotonic per `CatalogVersion`** (ignored until its `CatalogVersionPublished` + warm-completion marker); a rating run **MUST** pin one `CatalogVersion` for its entire duration, and at pin time the pinned version **MUST NOT** lag the newest completed version by more than 5s.

**Rationale**: Deterministic, monotonic resolution is the basis of reproducible rating.

**Actors**: `cpt-cf-bss-plan-price-actor-tariffs`

#### Pricing snapshot on billable artifacts

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-pricing-snapshot`

Charge artifacts and `BillableItem`s **MUST** carry catalog refs and `pricingSnapshotRef` per manifest §4.1; posted invoice periods **MUST NOT** re-query mutable catalog rows. Publish **MUST** stamp identifiers sufficient for the manifest `pricingSnapshotRef`.

**Rationale**: Reproducibility and posted-financial immutability require a complete frozen snapshot ref.

**Actors**: `cpt-cf-bss-plan-price-actor-rating`

### 6.8 Plan Lifecycle: Versioning, Retirement, Migration

#### Plan versioning and immutable history

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-plan-versioning`

A price or tier change on a published Plan **MUST** version the Plan and create **new** immutable `Price` records (prior rows retained as history); existing subscriptions **MUST** continue on their frozen snapshot until renewal or migration. Subscriptions bound via `existing_grandfathered` are the exception: they are **live-resolved** against an immutable grandfathered row (§17.5).

**Rationale**: Immutable price history and frozen snapshots protect active subscribers from silent repricing.

**Actors**: `cpt-cf-bss-plan-price-actor-finance-manager`

#### Published rows are append-only

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-published-rows-append-only`

A **published** price row on the canonical scope key **MUST NOT** be deleted (whether or not subscriptions are bound); published rows are append-only history. The operator **MUST** use supersession + grandfathering (or retirement + migration) instead. Deletion is available **only** for `draft` rows that were never published.

**Rationale**: Quotes, previews, exports, and overlays reference published rows; there is no deletion event to fan out.

**Actors**: `cpt-cf-bss-plan-price-actor-finance-manager`

#### Supersession within a scope key

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-supersession`

Supersession is versioning scoped to one canonical scope key: it **MUST** create a new immutable `Price` row (never mutate in place) and open/close the corresponding `PriceWindow` rather than overlap it, operating within one `priceEligibility` class and one `chargeKind`. An `existing_grandfathered` row is **immutable in price** and **MUST NOT** be superseded; the only permitted mutation is setting or **tightening** `grandfatherUntil` (never loosening, never the price), which is a material change.

**Rationale**: Scope-key-bounded supersession preserves non-overlap and grandfathered-price stability.

**Actors**: `cpt-cf-bss-plan-price-actor-finance-manager`

#### Plan retirement

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-plan-retirement`

Retirement **MUST** block **new** subscriptions to that `planId`, preserve existing subscription snapshots, emit `PlanRetired`, and **trigger the effective-dating-owned cancellation flow** (emitting `PriceWindowCancelled` per cancelled not-yet-active window, driving its cache-eviction path) — it **MUST NOT** merely mark windows invalid. The operator **MUST** be warned of cancelled windows before confirm.

**Rationale**: Safe retirement blocks new sales while preserving in-flight subscribers and cleaning future windows.

**Actors**: `cpt-cf-bss-plan-price-actor-finance-manager`

#### Scheduled migration

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-scheduled-migration`

Scheduled migration to a published target Plan **MUST** emit `PlanMigrationScheduled` for Subscriptions to create effective-dated `PlanLink`s without posted invoice mutation. Retry **MUST** be idempotent (no duplicate `PlanLink` requests for already-processed subscriptions) and cancellation before the effective date **MUST** invalidate the scheduled event without affecting already-migrated subscriptions.

**Rationale**: Migration must be re-trigger-safe, cancellable, and free of posted-invoice mutation.

**Actors**: `cpt-cf-bss-plan-price-actor-subscriptions`

#### Migration safety deltas and legacy snapshot synthesis

- [ ] `p2` - **ID**: `cpt-cf-bss-plan-price-fr-migration-safety`

Migration configuration **MUST** exclude contract-locked subscriptions (reported, lock never broken) and surface entitlement deltas and add-on deltas (subscribers whose add-ons become invalid, or who lack a required add-on) as blocking deltas. For a legacy subscription with no `pricingSnapshotRef`, the system **MUST** synthesize and freeze a snapshot from the published plan state **as of the trigger instant (UTC), frozen at execution**, recorded as `migrated-origin` with a provenance record (source `planId`/revision, resolved price ids, snapshot instant, trigger `migration`|`first-rating`, acting principal).

**Rationale**: Migrations must not break contract locks, overflow entitlements, or leave subscriptions un-snapshotted.

**Actors**: `cpt-cf-bss-plan-price-actor-subscriptions`

#### Contract-locked plan protection

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-contract-locked-protection`

While an active contract references a Plan revision, structural plan mutation **MUST** be rejected, directing the operator to a new Plan revision or contract expiry; contract-locked subscriptions **MUST** be excluded from scheduled migration.

**Rationale**: Contract-locked commercial terms must not be mutated out from under an active agreement.

**Actors**: `cpt-cf-bss-plan-price-actor-contracts`

#### Concurrent-edit conflict detection

- [ ] `p2` - **ID**: `cpt-cf-bss-plan-price-fr-concurrent-edit`

A submit with a stale version/ETag **MUST** be rejected with a conflict requiring refresh; an interactive edit targeting a row held under an in-flight bulk-import optimistic lock **MUST** fail with a conflict naming the bulk operation. The system **MUST NOT** silently overwrite either change.

**Rationale**: Optimistic concurrency prevents lost updates between bulk and interactive edits.

**Actors**: `cpt-cf-bss-plan-price-actor-finance-manager`

### 6.9 Consumer Contracts (Subscriptions / Rating / Billing)

#### Proration input contract

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-proration-input-contract`

Each recurring price row **MUST** publish `billingAnchorPolicy`, `prorationBasis` (the canonical enum, §1.4, adopted verbatim by Tariffs), and `creditOnDowngrade`, frozen in `pricingSnapshotRef`; proration math is owned by Subscriptions. A mid-cycle change crossing currency, region, or billing frequency **MUST** be rejected for in-place proration (handled as cancel + new subscription) and the operator **MUST** be warned that in-place credit is forfeited.

**Rationale**: Deterministic proration requires the same frozen inputs on the catalog and Subscriptions/Tariffs sides.

**Actors**: `cpt-cf-bss-plan-price-actor-subscriptions`

#### Billing timing on recurring rows

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-billing-timing`

Every recurring price row **MUST** carry `billingTiming` (`in_advance` | `in_arrears`), frozen in `pricingSnapshotRef`; usage rows are implicitly `in_arrears`. Publish **MUST** fail if a recurring row omits `billingTiming`; a hybrid plan **MAY** combine an `in_advance` base row with `in_arrears` usage rows (Billing derives deferral policy from this).

**Rationale**: Billing's deferral policy depends on an explicit, frozen billing timing.

**Actors**: `cpt-cf-bss-plan-price-actor-billing`

#### Entitlement grant set

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-entitlement-grant-set`

The published read model **MUST** include the plan's entitlement grant set (feature flags, quotas) or its `PlanTier`-resolved reference for Subscriptions provisioning; publish **MUST** fail if a referenced feature, quota, or `PlanTier` policy is undefined in the registry. This PRD does **not** define entitlement semantics.

**Rationale**: Subscriptions can only provision/enforce entitlements from a complete, published grant set.

**Actors**: `cpt-cf-bss-plan-price-actor-subscriptions`

#### Plan-change contract

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-plan-change-contract`

For a plan that participates in self-service changes, the read model **MUST** publish `allowedChangeTargets` (explicit target `planId`s or a rule) and a `comparabilityRank` (or an authoritative published `PlanTier` ordering) so Subscriptions can classify upgrade/downgrade/switch and constrain targets. Absence of `allowedChangeTargets` **MUST** mean **no self-service change** (fail-safe), not any-to-any; enforcement stays in Subscriptions.

**Rationale**: Subscriptions must not hard-code ordering or allow moves Finance never approved.

**Actors**: `cpt-cf-bss-plan-price-actor-subscriptions`

#### Rating compatibility contract

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-rating-compatibility`

Published plans **MUST** expose stable `{skuId, planId, priceId}` on all downstream artifacts, persist `modelKind` (and `quantitySource` for `per_unit`, `packageSize`/`packagePrice` for `package`), persist `tierAggregationWindow` and `billingGranularity` on usage rows at publish, and satisfy meter mapping (one `meteringUnit` per usage revision; a derived meter counts as one output unit). No monetary charge is computed here.

**Rationale**: Rating consumes frozen inputs; missing or defaulted fields would break deterministic charging.

**Actors**: `cpt-cf-bss-plan-price-actor-rating`

### 6.10 Advanced Pricing Primitives

#### Reserved-capacity price

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-reserved-capacity`

A `usage` price row **MAY** carry `reservedRate` and `reservationFlavor` (`consumption` | `capacity`) as attributes on the single row (not a second row), frozen in `pricingSnapshotRef` (§17.7). Tariffs evaluates the reservation (step 6) sourcing the self-service reserved rate from the catalog snapshot; the reserved/allocated quantity is supplied at runtime and the catalog **MUST NOT** meter, allocate, or compute the charge. Negotiated RI-style rates stay in Contracts.

**Rationale**: Self-service reserved rates need an authoring home without double-pricing the meter.

**Actors**: `cpt-cf-bss-plan-price-actor-tariffs`

#### Prepaid credit grant

- [ ] `p2` - **ID**: `cpt-cf-bss-plan-price-fr-prepaid-credit-grant`

A plan **MAY** declare a prepaid credit grant (`grantAmount` > 0, `creditUnit` = currency or **published** `meteringUnit`, `price` per `(currency, region)`, `expiryPolicy` = `never`|`days(N>0)`, `autoRechargeAllowed`), frozen in `pricingSnapshotRef` (§17.7). Publish **MUST** fail on an unset `expiryPolicy`, an unpublished `creditUnit`, or an unscoped grant price for a multi-`(currency, region)` plan; grant-price changes follow the material-change policy. The catalog **MUST NOT** persist any balance or compute drawdown.

**Rationale**: Wallet-style selling needs a defined grant primitive with balance execution owned by Billing/Rating.

**Actors**: `cpt-cf-bss-plan-price-actor-billing`

#### Derived (composite) meter

- [ ] `p2` - **ID**: `cpt-cf-bss-plan-price-fr-derived-composite-meter`

A price row **MAY** use a derived (composite) meter; publish **MUST** persist >= 2 **published** constituent `meteringUnit` ids, the formula expression **as data**, and a single **output unit**, and **MUST** fail if any constituent unit is unpublished or the formula self-references. The definition **MUST** be frozen in `pricingSnapshotRef`; Tariffs evaluates the formula and the catalog **MUST NOT** compute the result.

**Rationale**: Multi-unit offerings (e.g. VM = vCPU + RAM) need a composable, frozen definition without computing math here.

**Actors**: `cpt-cf-bss-plan-price-actor-tariffs`

#### Interim discount reference hook

- [ ] `p2` - **ID**: `cpt-cf-bss-plan-price-fr-discount-ref-hook`

An optional `discountRef` **MUST** be validated for referential integrity (resolves to a registered external discount instrument) and persisted on the snapshot; the catalog **MUST NOT** author, evaluate, or stack the discount, and its absence **MUST NOT** block publish. Evaluation is owned by Tariffs/Promotions.

**Rationale**: A day-1 discount hook must be sellable without the catalog owning discount authoring/evaluation.

**Actors**: `cpt-cf-bss-plan-price-actor-promotions`

#### Minimum-quantity floor typing

- [ ] `p2` - **ID**: `cpt-cf-bss-plan-price-fr-min-qty-floor`

A `minQtyThreshold` **MUST** declare its floor type — `purchase` (Subscriptions rejects orders below the floor) or `usage` (Tariffs/Rating treats usage below the floor as ineligible and fails closed to the plan's fallback, never silently zero-rated). Publish **MUST** reject a `minQtyThreshold` with no declared floor type and **MUST** warn if it falls inside a non-zero-priced band. The type + value are frozen in `pricingSnapshotRef`.

**Rationale**: An untyped floor is ambiguous between order-time rejection and downstream eligibility.

**Actors**: `cpt-cf-bss-plan-price-actor-finance-manager`

### 6.11 Operator Efficiency

#### Plan clone

- [ ] `p2` - **ID**: `cpt-cf-bss-plan-price-fr-plan-clone`

Clone **MUST** create a new `planId` in `draft` with copied configuration and **new** price ids, record `clonedFrom`, and **MUST NOT** affect source subscriptions. `priceEligibility` and `grandfatherUntil` **MUST** reset to defaults (not copied), contract locks **MUST NOT** be copied, and `discountRef` **MUST** be copied only if it still resolves to a registered instrument (else dropped with an operator notice).

**Rationale**: Cloning accelerates authoring without carrying eligibility/lock state that must be re-decided.

**Actors**: `cpt-cf-bss-plan-price-actor-finance-manager`

#### Bulk price import

- [ ] `p2` - **ID**: `cpt-cf-bss-plan-price-fr-bulk-price-import`

Bulk price import **MUST** validate all-or-nothing **pre-commit** (any invalid row blocks the whole batch with a per-row report); at **commit** it **MUST** take optimistic per-row locks and a per-row ETag conflict **MUST** fail only that row (batch reported partial; committed rows stand; conflicted rows listed for retry) — never silently overwrite.

**Rationale**: Annual repricing must be efficient yet safe against concurrent manual edits.

**Actors**: `cpt-cf-bss-plan-price-actor-finance-manager`

#### Mass repricing

- [ ] `p2` - **ID**: `cpt-cf-bss-plan-price-fr-mass-repricing`

A mass price adjustment over N rows **MUST** be idempotent (re-run-safe after partial failure) and emit deduplicated events; it **MAY** coalesce into one or few `CatalogVersion`s. The throughput SLO **MUST** be back-calculated from the tenant worst-case row count against an agreed maintenance window (provisional >= 50 rows/sec; Design confirms against the worst case).

**Rationale**: Large repricing runs must be safe to retry and must not flood consumers with duplicate events.

**Actors**: `cpt-cf-bss-plan-price-actor-finance-manager`

#### Mutation idempotency keys

- [ ] `p2` - **ID**: `cpt-cf-bss-plan-price-fr-mutation-idempotency`

A plan/price create/update call carrying a client idempotency key **MUST** return the original result on retry (no duplicate draft plan or price row) for a documented TTL; a key reused after the TTL **MAY** be a new request. An idempotency-key replay during an active bulk lock **MUST** return the original completed result regardless of current lock state.

**Rationale**: Safe retries require idempotent mutation semantics with a defined replay window.

**Actors**: `cpt-cf-bss-plan-price-actor-finance-manager`

#### Price history and export

- [ ] `p2` - **ID**: `cpt-cf-bss-plan-price-fr-price-history-export`

The system **MUST** return chronological immutable price-history records with actor and effective dates under Auditor/Finance filters and **MUST** support export within p95 <= 5s for 100 records.

**Rationale**: Auditors and Finance must reconcile contract and billing disputes from complete immutable history.

**Actors**: `cpt-cf-bss-plan-price-actor-auditor`

### 6.12 Access Control and Governance

#### RBAC deny-by-default

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-rbac-deny-by-default`

Plan mutate APIs **MUST** deny users lacking CatalogAdmin/FinanceReviewer/ProductManager/Auditor and audit-log attempts; **read/preview** APIs **MUST** also be deny-by-default (an unlisted-role principal is denied unless it holds an explicit **catalog-preview read grant**, region/brand-scoped by IdP claims). The historical-import (backdating) capability **MUST** be a distinct restricted grant, not a default role.

**Rationale**: Pricing data is commercially sensitive; both mutation and preview must be least-privilege.

**Actors**: `cpt-cf-bss-plan-price-actor-catalog-admin`

#### Tenant and brand isolation

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-tenant-brand-isolation`

The system **MUST** enforce tenant isolation and scope brand/region per IdP claims at the gateway (authorization/display scope). Pricing `region` is **decoupled** from the IdP authorization region: authoring or mutating a price row scoped to a pricing `region` the user's authz scope does not grant **MUST** be denied and audit-logged.

**Rationale**: Cross-tenant leakage is a critical incident class; authz region must not be conflated with pricing region.

**Actors**: `cpt-cf-bss-plan-price-actor-catalog-admin`

#### Historical-import (backdating) governance

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-historical-import-governance`

The historical-import path that permits past `availableFrom`/effective dates **MUST** require a **restricted role** and a **mandatory reason**, **MUST** be fully audited, and **MUST NOT** generate or re-open any downstream billable charge window. It is the **only** sanctioned backdated reference path (e.g. for legacy snapshot synthesis).

**Rationale**: Backdating is powerful and fraud-adjacent; it must be restricted, justified, audited, and side-effect-free downstream.

**Actors**: `cpt-cf-bss-plan-price-actor-catalog-admin`

#### Audit completeness and tamper-evidence

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-fr-audit-completeness`

Any plan/price mutation **MUST** record actor, timestamp, before/after version, and approval trail; price history **MUST** be retained **>= 7 years** (tenant/jurisdiction-configurable; the **maximum** applicable minimum where several apply) and stored **append-only / tamper-evident** (WORM or hash-chained) so prior versions cannot be mutated or deleted within the retention window.

**Rationale**: Tamper-evidence and jurisdiction-aware retention are compliance gates.

**Actors**: `cpt-cf-bss-plan-price-actor-auditor`

## 7. Non-Functional Requirements

### 7.1 NFR Inclusions

> Provisional targets below (availability/DR, mass-repricing throughput, size caps, idempotency-key TTL) are working assumptions that MUST be ratified before Design lock (no bare placeholders may ship); see §15.

#### Plan mutation latency

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-nfr-mutation-latency`

Plan create or update (end-to-end persistence and validation) **MUST** complete within **2 seconds at p95**.

**Threshold**: p95 <= 2s for plan create/update.

**Rationale**: Operator authoring throughput depends on responsive mutation.

#### Price field validation latency

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-nfr-validation-latency`

A single price row save (currency/region/tier validation) **MUST** complete within **200ms at p95**.

**Threshold**: p95 <= 200ms per price-row validation.

**Rationale**: Inline validation must feel immediate in the tier/price editor.

#### Publish propagation

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-nfr-publish-propagation`

Plan definitions **MUST** be visible to Rating cache warming within **5 seconds at p95** of `CatalogVersionPublished`; the registry **MUST** bound the delay from `PlanPublished` (pending) to `CatalogVersionPublished` (committed) by a stated **max batching-delay SLO**.

**Threshold**: p95 <= 5s from `CatalogVersionPublished` to read-model visibility; max batching-delay SLO ratified with the registry.

**Rationale**: Rating and portal depend on timely propagation of committed catalog content.

#### Plan read latency

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-nfr-read-latency`

Catalog browse or plan-detail read served from the read model within a tenant partition **MUST** have p95 latency **under 100ms**, holding at the 20-currency floor.

**Threshold**: p95 < 100ms per tenant partition (at >= 20 currencies/plan).

**Rationale**: Rating and portal reads are on the hot path.

#### Event propagation

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-nfr-event-propagation`

`PlanCreated`/`PlanUpdated` (and the other producer events) **MUST** reach downstream consumers within **5 seconds at p95**, carry correlation/idempotency keys, and be safe to redeliver (at-least-once; consumers dedupe by idempotency key).

**Threshold**: p95 <= 5s to consumers; duplicate redelivery safe.

**Rationale**: Downstream cache warming and eligibility depend on timely, dedupable events.

#### Multi-currency scale

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-nfr-multi-currency-scale`

The system **MUST** support **at least 20 currencies per plan as a guaranteed floor** (tested at 20), and the read SLO (p95 < 100ms) **MUST** hold at that floor, not only at nominal load.

**Threshold**: >= 20 currencies/plan with read p95 < 100ms sustained.

**Rationale**: Global plans carry many per-market rows; the read path must not degrade.

#### Mass-repricing performance and idempotency

- [ ] `p2` - **ID**: `cpt-cf-bss-plan-price-nfr-mass-repricing-throughput`

A mass price adjustment over N rows **MUST** be idempotent (re-run-safe) and emit deduplicated events; the throughput SLO **MUST** be back-calculated from the tenant worst-case row count (plans x currencies x regions) against an agreed maintenance window.

**Threshold**: Provisional >= 50 rows/sec (Design confirms against the worst case, not the provisional figure).

**Rationale**: Annual/bulk repricing must complete within a maintenance window and be safe to retry.

#### Read-model availability and disaster recovery

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-nfr-availability-dr`

The published read model **MUST** meet a stated availability SLO and disaster-recovery objective; during a read-model outage, charge resolution **MUST NOT** resolve from an unavailable or partially-warmed read model and **MUST** fail closed (frozen `pricingSnapshotRef` resolution is unaffected).

**Threshold**: Provisional availability >= 99.9% per tenant partition; RPO <= 5 min; RTO <= 30 min (Architecture ratifies before Design).

**Rationale**: Rating has a hard real-time dependency on the read model; best-guess pricing under lag is a show-stopper.

#### Audit retention and tamper-evidence

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-nfr-audit-retention`

Audit and price-history records **MUST** be retained **>= 7 years** (tenant/jurisdiction-configurable; the **maximum** applicable minimum where several apply) and stored **append-only / tamper-evident** (WORM or hash-chained) so prior versions cannot be mutated or deleted within the retention window.

**Threshold**: >= 7-year retention (configurable); 100% append-only/tamper-evident storage.

**Rationale**: Financial audit and dispute resolution require immutable, long-lived history.

#### Data residency for price and audit data

- [ ] `p2` - **ID**: `cpt-cf-bss-plan-price-nfr-data-residency`

Price and audit/history data for a residency-bound tenant **MUST** stay within the configured jurisdiction's residency boundary and **MUST NOT** replicate a row outside **any** of its residency jurisdictions' boundaries, consistent with the jurisdiction-configurable retention.

**Threshold**: Zero rows replicated outside a residency jurisdiction boundary.

**Rationale**: Data-residency violations are compliance incidents.

#### Plan and tier size limits

- [ ] `p2` - **ID**: `cpt-cf-bss-plan-price-nfr-size-limits`

The system **SHOULD** enforce configurable soft caps on tier-band count per row and price-row count per plan, emitting a publish warning above the cap, to protect the read SLO and mass-repricing SLO from pathological plans.

**Threshold**: Provisional <= 100 tier bands/row, <= 500 price rows/plan (tenant-configurable; Design may tune).

**Rationale**: Unbounded plan size degrades the read and repricing hot paths.

### 7.2 NFR Exclusions

Explicit dispositions for domains not owned by this PRD (no silent omissions):

- **Tax computation / scheme-determination performance**: Not applicable — Tax Engine / Billing; the catalog persists only the display basis and `taxCategory` reference.
- **Proration / plan-change math performance**: Not applicable — Subscriptions owns proration math; this PRD publishes the inputs.
- **FX / overlay evaluation performance**: Not applicable — Tariffs owns FX and overlay stacking; the catalog performs no FX or evaluation.
- **Prepaid balance-ledger / drawdown performance**: Not applicable — Billing / Rating; the catalog defines the grant only, never tracks balance.
- **Frontend UX performance / accessibility (WCAG) / i18n rendering**: Not applicable to this backend PRD — owned by the corresponding frontend DESIGN (Presentation renders localization).

## 8. Five Quality Vectors Analysis

| **Quality Vector** | **Show-Stopper Requirements** | **Rationale** |
|--------------------|-------------------------------|---------------|
| **Efficiency** | Self-service plan/tier config; clone; bulk import; >= 90% reduction in manual IT for standard plans | Faster commercial packaging for partners and product lines |
| **Reliability** | Fail-closed publish; consumer-side read-model determinism + monotonicity per `CatalogVersion`; read-model availability/DR for Rating; cross-PRD conformance fixtures (tier-boundary, proration); fan-out atomicity (retry-to-SLO or `PlanPublishDegraded`); immutable price history; deterministic ids on all charges; migration without invoice mutation | Billing incidents from bad config, partial publish, read-model outage, or cross-PRD seam drift are show-stoppers |
| **Performance** | Read p95 under 100ms; publish fan-out under 5s; validation under 200ms for price save | Rating and portal depend on the catalog read path |
| **Security** | Two-person rule with segregation of duties (no self-approval); RBAC incl. scoped historical-import capability; tenant/brand/region isolation; audit on deny; append-only/tamper-evident audit and price history; per-jurisdiction data residency | Unauthorized or self-approved pricing change is a financial-fraud risk; tamper-evidence and residency are compliance gates |
| **Versatility** | One-time, recurring (monthly to custom cycles), usage, hybrid, per-seat; add-ons; bundles; 20+ currencies; phases; grandfathering; migration | Required for IaaS/PaaS/SaaS and marketplace |

**Note**: Plan retirement and migration contributes to **Reliability** and **Versatility**. Billing descriptors contribute to **Reliability** (invoice reproducibility).

## 9. Public Library Interfaces

> Plan & Price Modeling is a backend catalog capability, not a client library. Interfaces below are high-level contracts; concrete API schemas, endpoints, and DDL belong in DESIGN.

### 9.1 Public API Surface

#### Plan/Price authoring and publish contract

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-interface-authoring-publish`

**Type**: conceptual authoring + publish contract (shape in Design)

**Stability**: stable (contract intent), schema unstable (Design owns)

**Description**: Create/update/clone plans and price rows in `draft`, run fail-closed publish validation, submit for approval (two-person rule), and publish — emitting the frozen event set and requesting a `CatalogVersion` increment. Accepts client idempotency keys; enforces optimistic concurrency (ETag).

**Breaking Change Policy**: Major version bump for incompatible request/response changes; the publish-validation and event-name contracts are part of the contract.

#### Published catalog read model

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-interface-catalog-read-model`

**Type**: conceptual read-model contract (shape in Design)

**Stability**: stable (contract intent), schema unstable (Design owns)

**Description**: Exposes, per committed `CatalogVersion`, the complete plan/price read model — `{skuId, planId, priceId}`, model kind, ordered tier bands, evaluation-policy fields, phase->price map, billing descriptors, proration/plan-change/entitlement contracts, reserved-rate and derived-meter definitions — resolvable via `pricingSnapshotRef`, monotonic per version, no draft reads.

**Breaking Change Policy**: Additive fields only within a major version; removing or redefining a published field is a breaking change.

#### Effective catalog price preview

- [ ] `p2` - **ID**: `cpt-cf-bss-plan-price-interface-price-preview`

**Type**: conceptual preview contract (shape in Design)

**Stability**: stable (contract intent), schema unstable (Design owns)

**Description**: Returns the catalog **base** list price, `taxInclusive` flag, tier summary, and trial length (`displayTrialDays`) for a `(region, currency)`; fails closed if no row exists (no implicit FX); indicates that Contract/`PriceList` overlays apply at purchase (Tariffs). Requires a catalog-preview read grant.

**Breaking Change Policy**: Additive; the base-price-only semantics (no overlay evaluation) are part of the contract.

### 9.2 External Integration Contracts

#### Tariffs read-model contract

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-contract-tariffs-readmodel`

**Direction**: provided by this PRD to Tariffs

**Protocol/Format**: model kind, ordered tier bands, `tierAggregationWindow`, `billingGranularity`, `priceEligibility`/`grandfatherUntil`, reserved-rate/flavor, derived-meter definition, `customerGroup` `PriceList` + resolved membership; all frozen in `pricingSnapshotRef` (Design).

**Compatibility**: No default substitution; absence of a required field must have failed publish; Tariffs never reads draft state.

#### Subscriptions publish contract

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-contract-subscriptions`

**Direction**: provided by this PRD to Subscriptions

**Protocol/Format**: eligible plans; migration targets + `PlanMigrationScheduled`; phase map + `convertsToPhaseId` + `displayTrialDays`; proration input contract (`billingAnchorPolicy`, `prorationBasis`, `creditOnDowngrade`); plan-change contract (`allowedChangeTargets`, `comparabilityRank`); entitlement grant set; sellability gate (active-window + committed-version check) (Design).

**Compatibility**: Catalog publishes inputs; Subscriptions executes proration/runtime/`PlanLink`; cross-currency/region/frequency mid-cycle change is unsupported at launch (cancel + new).

#### Registry CatalogVersion increment contract

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-contract-registry-catalogversion`

**Direction**: bidirectional with the catalog registry

**Protocol/Format**: on `PlanPublished` the catalog requests addressability; the registry (sole incrementer) MAY batch approved publishes into one discretionary catalog publish and emits `CatalogVersionPublished`; `pricingSnapshotRef` pins the committed version (Design).

**Compatibility**: Pending-ref on `PlanPublished`; committed version pinned in the snapshot; increment-trigger taxonomy + max batching-delay SLO ratified with the registry (Open Questions).

#### Billing descriptor contract

- [ ] `p2` - **ID**: `cpt-cf-bss-plan-price-contract-billing-descriptors`

**Direction**: provided by this PRD to Billing

**Protocol/Format**: billing descriptor set (invoice line template, tax category, `glCode`, itemization rule) frozen into `CatalogVersion`; `billingTiming` per recurring row (Design).

**Compatibility**: Sufficient to render an invoice line and post to GL **without** re-querying mutable catalog rows; the agreed minimum field list is fixed with Billing/Payments in Design.

#### PriceWindow linkage contract

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-contract-pricewindow`

**Direction**: bidirectional with the effective-dating price-windows use case

**Protocol/Format**: this PRD attaches windows to price rows and validates publish-time coverage; the use case owns scheduling, activation jobs, and `PriceWindowScheduled`/`Activated`/`Cancelled`/`Expired` mechanics (Design).

**Compatibility**: Every billable row is window-linkable; retirement triggers the use-case cancellation flow; the canonical scope key (§2.2) is the shared non-overlap key.

## 10. Use Cases

#### Guided plan creation

- [ ] `p2` - **ID**: `cpt-cf-bss-plan-price-usecase-plan-creation`

**Actor**: `cpt-cf-bss-plan-price-actor-finance-manager`

**Preconditions**:
- A published SKU exists.

**Main Flow**:
1. Select the published SKU.
2. Choose billing cycle (one-time / recurring / usage / hybrid) and frequency (monthly to custom).
3. Set `PlanTier` and optional ordered phases (trial to evergreen).
4. Configure base price and region/currency scope (per-seat: unit price + quantity source).
5. Add tiers, evaluation policy, or add-on rules.
6. Review the validation summary and submit for approval.

**Postconditions**:
- A complete plan draft is staged, pending approval.

**Alternative Flows**:
- **Fail-closed validation**: ambiguous meter, overlapping tiers, missing `PlanTier`/descriptors, or missing evaluation-policy fields block submission with a field-level report.

#### Tiered pricing editing

- [ ] `p2` - **ID**: `cpt-cf-bss-plan-price-usecase-tier-editor`

**Actor**: `cpt-cf-bss-plan-price-actor-finance-manager`

**Preconditions**:
- A usage plan price row exists.

**Main Flow**:
1. Open the usage plan price row.
2. Add tier rows with bounds and unit prices.
3. Select graduated vs volume.
4. Set `tierAggregationWindow` and `billingGranularity`.
5. Resolve inline validation; save draft or submit.

**Postconditions**:
- A validated, model-kind-explicit tier set is persisted.

**Alternative Flows**:
- **Overlap/gap/non-ascending bands**: save/publish fails; a closed top band warns and fails closed at evaluation.

#### Hybrid plan composition

- [ ] `p2` - **ID**: `cpt-cf-bss-plan-price-usecase-hybrid-composer`

**Actor**: `cpt-cf-bss-plan-price-actor-finance-manager`

**Preconditions**:
- A usage-capable published SKU exists.

**Main Flow**:
1. Select the usage-capable SKU.
2. Add a recurring base price section.
3. Add a usage price section with tiers or a flat rate.
4. Validate both sections present.
5. Submit for approval.

**Postconditions**:
- A hybrid plan with both components under one `planId` is staged.

**Alternative Flows**:
- **Incomplete hybrid**: publish validation fails until both a recurring and a usage row exist.

#### Plan approval

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-usecase-approval`

**Actor**: `cpt-cf-bss-plan-price-actor-finance-reviewer`

**Preconditions**:
- A plan is submitted for publish and flagged material (above threshold or first publish).

**Main Flow**:
1. Open the pending approvals queue.
2. Review the plan diff and threshold flag.
3. Approve or reject with reason.
4. The system records the independent approver (submitter + 1 approver).

**Postconditions**:
- On approval `PlanPublished` is emitted; on rejection the plan returns to `draft` with reason and the submitter is notified.

**Alternative Flows**:
- **Self-approval attempt**: rejected and audit-logged (submitter cannot approve).

#### Plan retirement and migration

- [ ] `p1` - **ID**: `cpt-cf-bss-plan-price-usecase-retire-migrate`

**Actor**: `cpt-cf-bss-plan-price-actor-finance-manager`

**Preconditions**:
- A plan with active subscriptions and a published target plan exist.

**Main Flow**:
1. View the active subscription count.
2. Choose immediate or scheduled retirement.
3. Select the target plan and migration date.
4. Review cancelled future windows and flagged deltas (contract-locked, entitlement overflow, invalid/missing-required add-ons).
5. If cross-currency/region/frequency: confirm the cancel+new warning (in-place credit forfeited).
6. Confirm the migration event.

**Postconditions**:
- `PlanRetired` (and, on schedule, `PlanMigrationScheduled`) is emitted; Subscriptions creates effective-dated `PlanLink`s; posted invoices are untouched.

**Alternative Flows**:
- **Contract-locked subscribers**: excluded and reported; the lock is never broken.

#### Bulk price import

- [ ] `p2` - **ID**: `cpt-cf-bss-plan-price-usecase-bulk-import`

**Actor**: `cpt-cf-bss-plan-price-actor-finance-manager`

**Preconditions**:
- A valid CSV with plan id, currency, region, amount, and taxInclusive columns.

**Main Flow**:
1. Download the CSV template.
2. Upload the filled file.
3. Review the validation summary.
4. Apply, or fix errors and retry.

**Postconditions**:
- Valid rows commit under optimistic per-row locks; conflicted rows are reported partial for retry.

**Alternative Flows**:
- **Any invalid row**: the whole batch is blocked pre-commit with a per-row report.

#### Price history and export

- [ ] `p2` - **ID**: `cpt-cf-bss-plan-price-usecase-price-history`

**Actor**: `cpt-cf-bss-plan-price-actor-auditor`

**Preconditions**:
- A plan with price change history exists.

**Main Flow**:
1. Open the plan price history.
2. Filter by currency/region/date.
3. View the trend and actors.
4. Export CSV/JSON.

**Postconditions**:
- Chronological immutable records are returned and exported (p95 <= 5s for 100 records).

#### Effective price preview

- [ ] `p2` - **ID**: `cpt-cf-bss-plan-price-usecase-price-preview`

**Actor**: `cpt-cf-bss-plan-price-actor-partner`

**Preconditions**:
- A published plan and a partner catalog-preview read grant (region/currency) exist.

**Main Flow**:
1. Select the plan in the catalog.
2. The system resolves the base price for the region/currency (fail-closed if no row — no implicit FX).
3. Show the tax-inclusive label, tier summary, and trial length (`displayTrialDays`) if applicable.
4. Show the disclaimer that Tariffs overlays apply at purchase.

**Postconditions**:
- The partner sees the catalog base list price with an overlay disclaimer.

**Alternative Flows**:
- **No row for `(currency, region)`**: preview fails closed (no FX fallback).

## 11. User Interaction and Design

| **Interface Name** | **Role** | **Steps** | **Mockup Screen** |
|--------------------|----------|-----------|-------------------|
| Plan creation wizard | As a Finance Manager, I want a guided flow to define billing cycle, base price, and add-ons so that plans are complete before publish | 1. Select published SKU<br>2. Choose billing cycle (one-time / recurring / usage / hybrid) and frequency (monthly…custom)<br>3. Set PlanTier and optional ordered phases (trial→evergreen)<br>4. Configure base price and region/currency scope (per-seat: unit price + quantity source)<br>5. Add tiers, evaluation policy, or add-on rules<br>6. Review validation summary and submit for approval | — |
| Tiered pricing editor | As a Finance Manager, I want to edit tier bands and model kind so that usage plans rate correctly | 1. Open usage plan price row<br>2. Add tier rows with bounds and unit prices<br>3. Select graduated vs volume<br>4. Set tier aggregation window and billing granularity<br>5. Inline validation; save draft or submit | — |
| Hybrid plan composer | As a Finance Manager, I want recurring base plus usage on one plan so that hybrid SKUs are modeled once | 1. Select usage-capable SKU<br>2. Add recurring base price section<br>3. Add usage price section with tiers or flat rate<br>4. Validate both sections present<br>5. Submit for approval | — |
| Billing descriptors panel | As a Finance Manager, I want to set invoice and GL fields on the plan so that Billing renders lines correctly | 1. Open plan publish checklist<br>2. Complete invoice template, tax category, GL code<br>3. Set bundle/add-on itemization if applicable<br>4. Resolve validation gaps | — |
| Add-on configuration | As a Product Manager, I want add-ons with dependency rules so that subscription composition is constrained | 1. Open plan detail<br>2. Search published add-on SKUs<br>3. Set required/optional and quantity bounds<br>4. Optional price override<br>5. Save and validate compatibility | — |
| Bundle builder | As a Product Manager, I want bundles with rev-share so that marketplace packages are sellable | 1. Select included SKUs and quantities<br>2. Set min/max bundle constraints<br>3. Configure rev-share and itemization<br>4. Set bundle price; review savings vs sum of parts<br>5. Publish bundle | — |
| Plan approval dashboard | As a Finance Reviewer, I want to approve or reject plan publishes so that material pricing changes are controlled | 1. Open pending approvals queue<br>2. Review plan diff and threshold flag<br>3. Approve or reject with reason<br>4. Track the independent approver when required (submitter + 1 approver) | — |
| Plan retirement and migration | As a Finance Manager, I want to retire a plan and schedule migration so that legacy pricing sunsets safely | 1. View active subscription count<br>2. Choose immediate or scheduled retirement<br>3. Select target plan and migration date<br>4. Review cancelled future price windows AND flagged deltas (contract-locked, entitlement overflow, invalid/missing-required add-ons)<br>5. If cross-currency/region/frequency: confirm the cancel+new warning (in-place credit forfeited)<br>6. Confirm migration event | — |
| Price history and export | As an Auditor, I want price history so that I can reconcile contract and billing disputes | 1. Open plan price history<br>2. Filter by currency/region/date<br>3. View trend and actors<br>4. Export CSV/JSON | — |
| Bulk price import | As a Finance Manager, I want bulk price updates so that annual adjustments are efficient | 1. Download CSV template<br>2. Upload filled file<br>3. Review validation summary<br>4. Apply or fix errors and retry | — |
| Effective price preview | As a Partner, I want list price preview so that I can quote customers | 1. Select plan in catalog<br>2. System resolves base price for region/currency (fail-closed if no row — no implicit FX)<br>3. Show tax-inclusive label, tier summary, and trial length (`displayTrialDays`) if applicable<br>4. Disclaimer for Tariffs overlays | — |

## 12. Acceptance Criteria

> **As a** Finance Manager or Product Manager **I want** to model subscription plans and price structures in the catalog **so that** Subscriptions can sell them and Rating can charge deterministically.

### Plan definition and billing cycles

**1. Create recurring monthly plan**
- **Given** a published SKU without usage metering (or with metering ignored for pure recurring)
- **When** a Finance Manager creates a Plan with `billingCycle=recurring`, frequency `monthly`, and a base Price row
- **Then** the system MUST store the Plan in `draft` (`PlanTier` is optional at draft and REQUIRED at publish — AC #7; it is not an input to this creation step, so creation MUST NOT demand it)
- **And** MUST validate non-negative amount (>= 0; `0` allowed for free/trial tiers), ISO 4217 currency, and active region scope

**2. Create recurring annual plan**
- **Given** a published SKU and FinanceReviewer role
- **When** a Finance Manager creates a Plan with frequency `annual` and base Price row
- **Then** the system MUST persist annual billing cycle metadata on the Plan
- **And** MUST allow independent price rows per currency/region for the same plan

**3. Usage-based plan with pricing tiers**
- **Given** a published SKU with `meteringUnit` configured
- **When** a Finance Manager configures tier bands and selects `modelKind` `graduated` or `volume`
- **Then** the system MUST reject overlapping quantity ranges
- **And** MUST persist tier definitions, `modelKind`, `tierAggregationWindow`, and `billingGranularity`

**4. One-time plan constraints**
- **Given** a Plan with `billingCycle=one-time`
- **When** add-on rules include recurring-only add-on SKUs
- **Then** publish validation MUST fail with a clear business error
- **And** MUST NOT emit `PlanPublished` until resolved

**5. One-time availability and quantity**
- **Given** a one-time Plan
- **When** a Finance Manager sets `availableFrom`/`availableTo` and min/max purchase quantity
- **Then** the system MUST reject `availableFrom` in the past (relative to **UTC** per manifest) for new plans, **except** via an explicit historical-import path that MAY set past dates (governed by AC #65 — restricted role, audited, no downstream charge windows)
- **And** MUST reject `minQty > maxQty`

**6. Hybrid plan (recurring + usage)**
- **Given** a published SKU with `meteringUnit`
- **When** a Finance Manager defines a hybrid Plan with both a recurring base Price and a usage Price row
- **Then** publish validation MUST pass only when both components exist
- **And** the read model MUST expose both rows under one `planId` for Tariffs hybrid evaluation

### Plan composition, phases, and descriptors

**7. Mandatory PlanTier**
- **Given** a Plan in `draft` without `PlanTier`
- **When** publish is requested
- **Then** publication MUST be blocked
- **And** the operator MUST receive a field-level validation message
- **And** publish MUST **also** block when the Plan's `PlanTier` differs from its parent SKU's `PlanTier` unless an explicit, audited override is set (plan-vs-SKU tier consistency)

**8. Plan phase price schedule (catalog)**
- **Given** a recurring Plan with configured, ordered phases (trial, intro, evergreen)
- **When** the Plan is published
- **Then** the catalog read model MUST map each phase id to applicable price row references and persist phase ordering with each phase's `convertsToPhaseId` successor
- **And** Subscriptions MUST be able to resolve phase→price and trial-to-paid binding without reading draft state
- **And** trial **runtime** mechanics (duration, payment-method requirement, auto-convert vs. expire) remain owned by Subscriptions Trial Management

**9. Billing descriptor completeness**
- **Given** a Plan submitted for publish
- **When** invoice line template, tax category, or GL mapping is missing per manifest minimum set
- **Then** publication MUST be blocked
- **And** the validation report MUST list missing descriptor fields

**10. Bundle itemization mode**
- **Given** a Bundle with multiple included SKUs
- **When** a Product Manager sets `invoiceItemization=aggregate` or `itemize`
- **Then** the system MUST persist the mode on the Bundle
- **And** published snapshots MUST include itemization rule for Billing line generation

### Pricing tiers and usage policy

**11. Graduated vs volume distinction**
- **Given** identical numeric tier bands, including a `$0` first band (`fromQty=0`)
- **When** one price row is `modelKind=graduated` and another is `modelKind=volume`
- **Then** both MUST publish successfully
- **And** the read model MUST persist distinct model kinds for Tariffs (no inference at rating time)
- **And** a total quantity falling entirely within the `$0` first band MUST resolve to a `0` charge under both kinds (no implicit minimum)

**12. Tier band validation**
- **Given** tier bands on a usage price row following the `[fromQty, toQty)` convention
- **When** bands overlap, leave coverage gaps, or `fromQty` is not ascending
- **Then** save/publish MUST fail
- **And** the top band MUST be open-ended (`toQty=null`) unless an explicit maximum is intended
- **And** a **closed** top band (explicit maximum) MUST emit a publish-time warning; quantity exceeding a closed top band MUST **fail closed** at evaluation (Tariffs/Rating; no free, no clamp) — the read model persists the explicit maximum and the fail-closed marker (conformance covered by AC #60)
- **And** SHOULD warn (advisory, non-blocking) when any band's effective unit price exceeds the previous band's (non-volume-discount pattern)

**13. Tier aggregation window required**
- **Given** a tiered usage price row
- **When** `tierAggregationWindow` is unset at publish
- **Then** publication MUST fail
- **And** the error MUST reference allowed values per Glossary

**14. Billing granularity required**
- **Given** a usage price row (tiered or flat usage)
- **When** `billingGranularity` is unset at publish
- **Then** publication MUST fail
- **And** the published row MUST be consumable by Tariffs step 3

**15. Flat usage unit price**
- **Given** a usage SKU without tier bands
- **When** a Finance Manager sets `modelKind=flat` and unit price
- **Then** publish MUST succeed with single-rate read model
- **And** tier fields MUST remain empty, but `billingGranularity` is still REQUIRED (per AC #14) — flat usage is not exempt from evaluation-policy fields

### Multi-currency, tax display, and price rows

**16. Multiple currencies on one plan**
- **Given** an existing Plan with USD price for region US
- **When** a Finance Manager adds EUR price for region EU on the same plan
- **Then** both price rows MUST be stored linked to the same `planId`
- **And** MUST emit `PriceCreated` per row (frozen event name — see §4 Event alignment; no aliases)

**17. Duplicate scope-key rejection**
- **Given** a Plan with a published price row on a canonical scope key `(planId, currency, region, priceList, phase, priceEligibility, chargeKind)`
- **When** a second row is created on the **same** canonical scope key without the supersession workflow
- **Then** the system MUST reject the duplicate
- **And** MUST suggest update of the existing row; rows differing on **any** key axis — e.g. a `trial`-phase price, an `existing_grandfathered` row (AC #25), or the `usage` vs `recurring` component of a hybrid plan — are **distinct**, not duplicates

**18. Tax-inclusive flag**
- **Given** a price row with `taxInclusive=true` for a region with configured tax rate
- **When** the price is displayed or published
- **Then** the system MUST persist `taxInclusive=true` on the snapshot
- **And** MUST show tax-included labeling in preview UI; tax calculation remains downstream
- **And** because Tax Engine is **post-MVP** (§13 External dependencies), at MVP a plan with `taxInclusive=true` MAY be authored but MUST be flagged **not-sellable-GA** until Tax Engine GA (tax-inclusive validation + `region`→jurisdiction depend on it); MVP sells tax-exclusive

**19. Tax-inclusive without tax rate**
- **Given** `taxInclusive=true` for a region without tax rate configuration
- **When** publish is requested
- **Then** publication MUST fail or require explicit override per the **tenant tax-display policy** (default fail-closed; see §17.6 Tenant policy objects)
- **And** tax **scheme** determination (VAT/GST/US sales tax) remains owned by Tax Engine — catalog persists only display basis and `taxCategory`

### Add-ons and bundles

**20. Add-on dependency rules**
- **Given** a base Plan and published add-on SKUs
- **When** add-on rules are saved with required/optional and min/max/step
- **Then** the system MUST validate SKU compatibility with the base SKU
- **And** MUST reject conflicting add-on pairs before publish

**21. Required add-on quantity bounds**
- **Given** an add-on marked required
- **When** `maxQty=0` is configured
- **Then** validation MUST fail
- **And** MUST NOT allow save

**22. Add-on price override reference**
- **Given** an add-on rule with catalog price override (discounted vs standalone)
- **When** the rule is saved
- **Then** the system MUST persist override reference on the plan snapshot
- **And** SHOULD warn (advisory, non-blocking) when the override unit price exceeds the add-on's standalone list price

**23. Bundle with rev-share**
- **Given** a marketplace Bundle with vendor and platform SKUs
- **When** rev-share percentages are configured (parties = the platform cut + one or more vendor shares)
- **Then** all shares MUST sum to 100% per included vendor SKU, with the platform cut being one **explicit** share (not an implicit remainder)
- **And** the operator MUST nominate a **primary party** at bundle authoring (default = the platform) that absorbs any rounding residual (e.g. 33.33%×3) within a <= 0.01% tolerance; publish MUST fail if shares do not reconcile to 100%
- **And** rev-share is scoped per `(bundle, vendor SKU)` — the same SKU in multiple bundles carries independent shares; `invoiceItemization=aggregate` MUST still preserve per-SKU rev-share for Marketplace accrual

### PriceWindow linkage

**24. Publish requires window coverage**
- **Given** a billable Plan with price rows for `(currency, region)` but no active or scheduled `PriceWindow`
- **When** publish is requested
- **Then** publication MUST fail for production sellability
- **And** MUST direct operator to schedule window per the effective-dating price-windows use case
- **And** for **one-time** plans the two effectivity mechanisms are distinct and both apply: a **`PriceWindow`** governs **price effectivity** (a purchase at `t` MUST have a covering window) while `availableFrom`/`availableTo` govern **purchasability** — when they disagree, purchase requires **both** an active window **and** an open availability interval (AC #5)

**25. Grandfathering eligibility on window/price**
- **Given** a price row with `priceEligibility=existing_grandfathered` and cutover timestamp
- **When** published
- **Then** the read model MUST expose eligibility metadata for Tariffs step 2
- **And** new subscriptions after cutover MUST NOT bind to grandfathered row
- **And** the grandfathered row and its successor row are **distinct canonical scope keys** (they differ by `priceEligibility`) and both MAY hold active `PriceWindow`s at the same `t` without violating non-overlap; Tariffs step 2 selects by `priceEligibility` + subscription `activatedAt`

### Publish validation, approval, and events

**26. Publish validation fail-closed (aggregate)**
- **Given** a Plan submitted for publication
- **When** validation detects any of: ambiguous meter, overlapping tiers, tier coverage gaps, **unspecified `modelKind` on a tiered row**, evaluation-policy fields present on a non-usage row, a `package` row with non-positive `packageSize`/negative `packagePrice`/tier bands present, a prepaid grant with unset `expiryPolicy` or an unpublished `creditUnit`, missing PlanTier, missing descriptors, invalid hybrid, missing evaluation-policy fields, currency precision exceeding the ISO 4217 minor unit, an unknown `region`, an unknown `brand` on a brand-scoped `PriceList`, an add-on dependency cycle, an uncovered gap between scheduled `PriceWindow` rows, a `customEveryN Days` cycle with a `calendar_month`/`fixed_day` anchor, a `PriceList` adjustment with an ambiguous tax basis, a rev-share reconciliation forcing a negative share, a configuration forcing mixed-currency lines on one invoice, a recurring row missing `billingTiming`, a one-time setup row that is not one-time (has recurrence, `billingTiming`, or tier fields), a `minQtyThreshold` with no declared floor type, a plan `PlanTier` unequal to its SKU `PlanTier` without an audited override, a `sellableFrom`/`sellableTo` not covered by an active or scheduled window, or a **required** scope key with no covering price row — the *required coverage set* = every canonical scope key that (a) has a scheduled `PriceWindow`, (b) is a `sum_of_parts` bundle component's sell scope, or (c) is an add-on/override target for a scope the base plan sells
- **Then** publication MUST be blocked
- **And** MUST NOT emit `PlanPublished` or warm Rating read models

**27. Successful publish fan-out**
- **Given** a Plan passing validation and required approvals
- **When** publication completes
- **Then** the system MUST emit `PlanPublished` carrying a **pending** `CatalogVersion` reference (the addressable version is committed by the registry, which MAY batch — see §17.5 CatalogVersion increment contract); the committed `CatalogVersion` and the `pricingSnapshotRef` composite are finalized when the registry emits `CatalogVersionPublished`
- **And** Rating/Tariffs MUST resolve `{skuId, planId, priceId}`, model kind, and evaluation-policy fields from the committed `CatalogVersion` without draft reads

**28. Two-person rule on material change**
- **Given** a material change — a price change above the configured threshold, **or** a first publish with no prior baseline (always material, per Glossary)
- **When** a Finance Manager submits for publish
- **Then** the system MUST require **one independent approver** before `PlanPublished` (submitter + 1 approver = **two distinct principals**; the submitter MUST NOT approve — AC #70)
- **And** MUST log the submitter and the approver identities and timestamps

**29. Approval rejection**
- **Given** a pending plan approval
- **When** a Finance Reviewer rejects with reason
- **Then** the Plan MUST return to `draft`
- **And** MUST emit audit event with rejection reason; submitter MUST be notified

**30. Auto-publish below threshold**
- **Given** a plan change and a tenant approval-threshold policy
- **When** publish is requested
- **Then** the system MUST apply the two-person rule **by default** (see AC #28; auto-publish is the **only** exception), and MAY auto-publish with **no independent approver** **only if** a threshold is explicitly configured **and** the change is below it **and** it is not a first publish (no configured threshold, or a first publish, implies two-person rule always applies)
- **And** in either path the system MUST emit `PlanPublished` and an audit record

**31. Pricing snapshot on subscription artifacts**
- **Given** an active subscription from a published Plan
- **When** Subscriptions or Rating generates charge artifacts
- **Then** artifacts MUST carry catalog refs and `pricingSnapshotRef` per manifest §4.1
- **And** posted invoice periods MUST NOT re-query mutable catalog rows

### Plan lifecycle, update, and migration

**32. Plan update with active subscribers**
- **Given** a published Plan with active subscriptions
- **When** price or tier configuration changes
- **Then** the system MUST version the Plan and create new Price records (immutable history)
- **And** existing subscriptions MUST continue on frozen snapshot until renewal or migration
- **And** exception: subscriptions bound via `existing_grandfathered` are **live-resolved** by Tariffs against an **immutable** grandfathered row (MUST NOT be superseded; sunset only via `grandfatherUntil` — see §17.5), so their price stays stable and consistent with the frozen-snapshot doctrine

**33. Plan retirement — block new, preserve existing**
- **Given** a Plan with active subscriptions
- **When** immediate retirement is executed
- **Then** new subscriptions to that `planId` MUST be blocked
- **And** existing subscriptions MUST retain plan snapshots; catalog MUST emit `PlanRetired`

**34. Scheduled migration to alternative plan**
- **Given** a retiring Plan and published target Plan
- **When** migration is configured with effective date and target `planId`
- **Then** the system MUST emit the migration-scheduled event **`PlanMigrationScheduled`** (frozen event name — see §4 Event alignment; no aliases) for Subscriptions
- **And** Subscriptions MUST create effective-dated `PlanLink` without posted invoice mutation

**35. Retirement cancels future price windows**
- **Given** a Plan with scheduled future `PriceWindow` rows
- **When** the Plan is retired
- **Then** the system MUST cancel not-yet-active windows and MUST **trigger the use-case-owned cancellation flow, which emits `PriceWindowCancelled` per cancelled window** (event name and producer owned by the effective-dating price-windows use case; drives its cache-eviction path, <= 2s NFR) — it MUST NOT merely mark windows invalid without the cancellation flow
- **And** MUST warn the operator of cancelled windows before confirm

**36. Contract-locked plan**
- **Given** an active contract referencing a Plan revision
- **When** structural plan mutation is attempted
- **Then** the system MUST reject while lock is active
- **And** MUST direct operator to new Plan revision or contract expiry

**37. Concurrent edit detection**
- **Given** two Finance Managers editing the same Plan
- **When** the second submits with stale version/ETag
- **Then** the system MUST reject with conflict
- **And** MUST require refresh before retry

### Operator efficiency

**38. Plan clone**
- **Given** a source Plan (draft or published)
- **When** clone is executed with new name
- **Then** the system MUST create a new `planId` in `draft` with copied configuration and new price ids
- **And** MUST record `clonedFrom` reference; MUST NOT affect source subscriptions

**39. Bulk price import**
- **Given** a valid CSV with plan id, currency, region, amount, taxInclusive columns
- **When** bulk import is applied
- **Then** **validation** MUST be all-or-nothing **pre-commit** (any invalid row blocks the whole batch with a per-row report)
- **And** at **commit**, the system MUST take optimistic per-row locks; a per-row ETag conflict (concurrent manual edit) MUST fail **only that row** and the batch MUST be reported **partial** (committed rows stand; conflicted rows are listed for retry) — it MUST NOT silently overwrite

**40. Price history and export**
- **Given** a Plan with price change history
- **When** an Auditor requests history with filters
- **Then** the system MUST return chronological immutable records with actor and effective dates
- **And** MUST support export within p95 <= 5s for 100 records

### Price preview and access control

**41. Catalog base price preview**
- **Given** a published Plan and partner context (region, currency)
- **When** preview is requested
- **Then** the system MUST return catalog base price, taxInclusive flag, and tier summary if applicable
- **And** MUST indicate that Contract/PriceList overlays apply at purchase (Tariffs)

**42. RBAC deny-by-default**
- **Given** a user without CatalogAdmin, FinanceReviewer, ProductManager, or Auditor role
- **When** plan mutate APIs are called
- **Then** access MUST be denied
- **And** attempts MUST be audit-logged
- **And** **read/preview** APIs MUST also be **deny-by-default**: an unlisted-role principal (e.g. a Partner/SI holding none of CatalogAdmin/FinanceReviewer/ProductManager/Auditor) MUST be denied unless it holds an explicit **catalog-preview read grant** (a scoped role/claim); partner preview (AC #41) requires this grant, region/brand-scoped by IdP claims (AC #43)
- **And** the **historical-import (backdating) capability** is a distinct restricted grant (a scoped `CatalogAdmin` capability, NOT a default role) required by AC #65

**43. Tenant and brand isolation**
- **Given** two tenants with plans configured
- **When** a user queries or mutates plans
- **Then** the system MUST enforce tenant isolation
- **And** MUST scope brand/region per IdP claims at gateway (authorization/display scope; pricing `region` is decoupled — see AC #68); here `brand` is an **authorization/display** scope, **not** a price-row key (see §2.2 Canonical scope key)

### Commercial model coverage

**44. Per-seat (per-unit recurring) plan**
- **Given** a published SKU and a recurring Plan priced per unit (seat/user)
- **When** a Finance Manager sets `modelKind=per_unit`, a unit price, and `quantitySource` (`subscription_seat_count` \| `manual`)
- **Then** publish MUST persist the unit price and quantity source
- **And** the read model MUST let Rating/Tariffs resolve per-period quantity **from the source declared by `quantitySource`** — from Subscriptions when `subscription_seat_count`, or from the fixed value frozen on the row when `manual` — without inferring or metering it (no metering-unit footprint)

**45. Custom billing frequency**
- **Given** a recurring Plan
- **When** a Finance Manager sets `frequency` to `quarterly`, `semiannual`, or `customEveryN{Days|Months}(n)`
- **Then** the system MUST persist the interval as metadata
- **And** MUST reject a non-positive `n`

### Proration and provisioning contracts

**46. Proration input contract (catalog → Subscriptions)**
- **Given** a recurring price row submitted for publish
- **When** publication completes
- **Then** the read model MUST expose `billingAnchorPolicy`, `prorationBasis`, and `creditOnDowngrade`
- **And** these fields MUST be frozen in `pricingSnapshotRef`; proration math remains owned by Subscriptions
- **And** a mid-cycle change crossing currency, region, or billing frequency MUST be rejected for in-place proration (handled as cancel + new subscription; see §17.6 Proration input contract)

**47. Entitlement grant set in published read model**
- **Given** a Plan submitted for publish
- **When** validation runs
- **Then** the published read model MUST include the plan's entitlement grant set (feature flags, quotas) or its `PlanTier`-resolved reference for Subscriptions provisioning
- **And** publish MUST fail if a referenced feature, quota, or `PlanTier` policy is undefined in the registry

### Currency and FX policy

**48. Missing currency row fails closed (no implicit FX)**
- **Given** a preview or publish for a `(currency, region)` with no price row
- **When** resolution runs
- **Then** the system MUST fail closed and MUST NOT compute or apply FX conversion
- **And** a base-currency fallback MUST occur only when an explicit `currencyFallbackPolicy` is configured (Future scope)

### Lifecycle safety (contract lock and entitlement overflow)

**49. Scheduled migration vs contract-lock and entitlement overflow**
- **Given** a scheduled migration to a target Plan
- **When** some source subscriptions are contract-locked, or the target Plan's entitlement limits are below current usage/seat counts
- **Then** the system MUST exclude contract-locked subscriptions from the migration and report them, and MUST NOT break an active contract lock
- **And** the migration config MUST surface entitlement deltas and warn/block when target limits < entitlements in use; runtime enforcement is owned by Subscriptions
- **And** the migration config MUST flag subscribers whose current add-ons become invalid under the target plan's add-on rules (e.g. a now-incompatible add-on), AND subscribers who **lack a required add-on** declared by the target plan, reporting both as blocking deltas alongside entitlement deltas

### Read-model and publish robustness

**50. Consumer-side read-model resolution (determinism)**
- **Given** a published Plan with `modelKind=graduated`
- **When** Tariffs or Rating resolves the price input via `pricingSnapshotRef`
- **Then** it MUST receive the exact ordered tier bands, `tierAggregationWindow`, and `billingGranularity` as published
- **And** it MUST NOT read draft state and MUST NOT substitute any default for an absent evaluation-policy field (absence MUST have failed publish)

**51. Publish fan-out atomicity / compensation**
- **Given** a Plan passing validation and approval
- **When** `PlanPublished` is emitted but read-model warming or a downstream notification fails
- **Then**, once the version commits (`CatalogVersionPublished`), the system MUST either complete warming within the 5s SLO via retry, or mark the publish degraded and emit a `PlanPublishDegraded` signal; the pre-commit batching delay (`PlanPublished` → `CatalogVersionPublished`) is governed by the AC #95 max batching-delay SLO, **not** by `PlanPublishDegraded`
- **And** the system MUST NOT leave a state where charges can resolve against a partially-published plan

**52. Currency precision per ISO 4217 minor units**
- **Given** a price row in a currency with a non-2 minor unit (e.g. JPY=0, BHD=3)
- **When** an amount is saved with more precision than the currency's ISO 4217 minor unit
- **Then** publish MUST reject it
- **And** a `0`-minor-unit currency MUST reject any fractional amount; a 3-minor-unit currency MUST accept up to 3 decimals

**53. Billing/ERP descriptor sufficiency**
- **Given** a published billing descriptor set
- **When** Billing/ERP consumes the snapshot to render an invoice line and post to GL
- **Then** the descriptor set MUST contain the agreed minimum field set (invoice template, tax category, `glCode`, itemization rule) sufficient to post **without re-querying mutable catalog rows**
- **And** publish MUST fail if any field in that agreed minimum set is absent (field list fixed with Billing/Payments in Design)

### Phases and PriceList authoring

**54. Phase ordering integrity**
- **Given** a Plan with ordered phases referencing each other via `convertsToPhaseId`
- **When** publish runs
- **Then** the system MUST reject any `convertsToPhaseId` that is dangling (targets a non-existent phase) or forms a cycle
- **And** MUST require exactly one terminal phase (no successor, e.g. `evergreen`)

**55. PriceList authoring and precedence**
- **Given** a Catalog Admin authoring a `PriceList` row
- **When** scope (partner/orgTier/brand/region/**customerGroup**/global), adjustment type (markup/discount/fixed), and an explicit `precedence` value are set
- **Then** the system MUST persist the row and MUST reject a duplicate `precedence` within the same scope class (deterministic ordering for Tariffs)
- **And** precedence/stacking **evaluation** remains owned by Tariffs (authoring/validation only here)
- **And** a `PriceList` adjustment MAY be **effective-dated** via its own **adjustment-level effectivity interval** `[effectiveFrom, effectiveTo)` — **outside** the per-plan `PriceWindow` mechanism, since an adjustment is scope-wide and has no single `planId`/`phase`/`priceEligibility`/`chargeKind`; absent an interval it applies whenever the base row is active. Overlap is validated **per `PriceList` scope + adjustment target** at authoring (not on the canonical price-row key). Base price rows always carry `priceList = base` (see §2.2)

**56. PriceList references unpublished plan/SKU**
- **Given** a `PriceList` row whose scope references a plan or SKU that is not published
- **When** the row is saved or the list is published
- **Then** the system MUST reject it with a clear business error
- **And** MUST NOT expose the row in the read model

### Migration robustness and eligibility

**57. Migration idempotency and cancellation**
- **Given** a scheduled migration
- **When** the migration trigger is re-executed (retry) or the operator cancels before the effective date
- **Then** retry MUST be idempotent (no duplicate `PlanLink` requests for already-processed subscriptions)
- **And** cancellation MUST invalidate the scheduled migration event without affecting already-migrated subscriptions

**58. Legacy subscription migration without prior snapshot**
- **Given** a legacy subscription with no `pricingSnapshotRef`
- **When** it is migrated or first rated under this system
- **Then** the system MUST synthesize and freeze a snapshot from the published plan state **as of the trigger instant (UTC), frozen at execution** (not config time), recording it as `migrated-origin`: for the `migration` trigger the instant is the **migration effective timestamp**; for the **first-rating** trigger (no prior snapshot, not a migration) it is the **earliest unrated usage timestamp (UTC)** for the subscription — two implementations MUST freeze identical prices for the same subscription
- **And** MUST persist a provenance record containing at minimum: source `planId`/revision, resolved price ids, the snapshot instant (UTC), the trigger (`migration` \| `first-rating`), and the acting principal (exact field shape MAY be finalized in Design; this field list is normative)
- **And** the snapshot MAY reference a backdated price row created via the historical-import path (AC #65) — this is the **only** sanctioned backdated reference path; it MUST NOT silently rate against mutable catalog rows

**59. `new_subscriptions_only` eligibility binding**
- **Given** a price row with `priceEligibility=new_subscriptions_only` and a cutover timestamp
- **When** published
- **Then** new subscriptions created on/after cutover MUST bind to the row
- **And** existing subscriptions MUST NOT be re-bound to it (they retain their prior snapshot)

### Cross-PRD contract conformance

**60. Tariffs tier-boundary conformance**
- **Given** a published tier set under the `[fromQty, toQty)` half-open convention
- **When** Tariffs evaluates a charge at each band boundary value (including the `fromQty=0` `$0` band and the open-ended top band)
- **Then** the charge MUST match a jointly-owned golden conformance fixture exactly (no off-by-one at band edges)
- **And** publish MUST be blocked if the conformance fixture is absent for a newly introduced `modelKind`
- **And** `package` (repeating-block) and `per_unit` (external-quantity) MUST **each** have a joint golden fixture before publish (per §17.2); absent the fixture, plans of that kind MUST NOT publish (two Scope items stay unsellable until Tariffs ships the semantics)

**61. Proration field-consumption contract**
- **Given** each proration input field (`billingAnchorPolicy`, `prorationBasis`, `creditOnDowngrade`)
- **When** the publish contract is validated
- **Then** each field MUST map to a named consuming requirement in Subscriptions (Proration Logic) **and** in Tariffs, which consumes the **same** `prorationBasis` for PriceWindow-split / plan-change proration and MUST adopt the canonical enum **verbatim**
- **And** a shared golden fixture MUST assert that `calendar_days_30` day-capping and `fixed_day(31)`→February month-end UTC rollover produce identical results on the catalog and Subscriptions sides, **and** a boundary fixture MUST assert that a mid-period **PriceWindow-split** proration produces identical results on the catalog↔Tariffs side

**62. Read-model monotonicity and staleness bound**
- **Given** an in-flight publish fan-out
- **When** a consumer resolves a price within the 5s warm window
- **Then** it MUST resolve against a single committed `CatalogVersion` and MUST NOT observe a partially-warmed version (the read model is **monotonic per `CatalogVersion`**; a version MUST be ignored until its `CatalogVersionPublished` **and** warm-completion marker — under batching a version bundles many `PlanPublished` events, so there is no per-`PlanPublished` marker)
- **And** a rating run MUST pin **one** `CatalogVersion` for its **entire duration** (determinism); at **pin time** the pinned version MUST NOT **lag the newest completed `CatalogVersion` by more than 5s** (a relative freshness bound, not an absolute age), then it holds for the whole run (no mid-run swap). This reconciles AC #51 and the event-propagation NFR without making long runs non-reproducible

**63. CatalogVersion increment on publish**
- **Given** a Plan passing validation and approval
- **When** `PlanPublished` completes
- **Then** the plan's content MUST become addressable in a `CatalogVersion`, with the registry as the **sole** incrementer; the registry **MAY batch** multiple approved publishes into **one** discretionary catalog publish (**not** one dedicated version per `PlanPublished`), and `pricingSnapshotRef` MUST pin the exact version containing the plan (a mass repricing MAY coalesce into one/few versions — AC #101)
- **And** publish-contract sign-off MUST remain blocked until the registry owner confirms the increment-trigger taxonomy (per-publish vs batched; see §15 Open Questions)

### Approval, governance, and access control

**64. Material-change comparison for multi-currency changes**
- **Given** a price change touching multiple `(currency, region)` rows
- **When** the approval-threshold policy is evaluated
- **Then** each affected row's delta MUST be compared against the per-currency threshold **in its own currency**
- **And** if **any** row exceeds its threshold, the two-person rule MUST apply

**65. Historical-import (backdating) governance**
- **Given** the historical-import path that permits past `availableFrom`/effective dates (AC #5)
- **When** it is invoked
- **Then** it MUST require a **restricted role** and a **mandatory reason**, and MUST be fully audited (audit-completeness NFR fields)
- **And** it MUST NOT generate or re-open any downstream billable charge window

**66. Cross-boundary mid-cycle change — operator warning**
- **Given** a mid-cycle change crossing currency, region, or billing frequency (modeled as cancel + new subscription per AC #46)
- **When** the operator previews or configures the change
- **Then** the preview/migration UI MUST warn that in-place proration credit is **forfeited** and the change is modeled as cancel + new subscription
- **And** the operator MUST explicitly confirm before proceeding

**67. Trial duration published for preview/quoting**
- **Given** a Plan with a `trial` phase declaring an authored trial length
- **When** published
- **Then** the catalog read model MUST publish `displayTrialDays` so preview/quoting can show the trial length (e.g. "14-day trial")
- **And** Subscriptions MUST enforce trial runtime **from this same published value** (single source: catalog authors, Subscriptions enforces) — `displayTrialDays` is not a second runtime authority

**68. Cross-region authoring authorization**
- **Given** a user whose IdP authorization region claim is `EU`
- **When** the user attempts to author or mutate a price row scoped to pricing `region=US`
- **Then** access MUST be denied unless the user's authz scope explicitly grants the target pricing region (pricing `region` is independent of, and not implied by, the authz region claim)
- **And** the attempt MUST be audit-logged (per AC #42)

### Discounts and approval governance

**69. Interim discount reference (conditional day-1 hook)**
- **Given** the Promotions PRD is not GA-ready and typed credit rows are Future scope
- **When** a Plan is published with an optional `discountRef`
- **Then** the catalog MUST validate that `discountRef` resolves to a registered external discount instrument and MUST persist it on the snapshot
- **And** the catalog MUST NOT author, evaluate, or stack the discount (evaluation owned by Tariffs/Promotions); absence of `discountRef` MUST NOT block publish

**70. Self-approval prohibition (segregation of duties)**
- **Given** a Plan submitted for publish that requires the two-person rule (material change, AC #28)
- **When** an approver acts on the submission
- **Then** the submitter's identity MUST NOT satisfy the required approval slot (the submitter and the independent approver MUST be **distinct principals**)
- **And** any self-approval attempt MUST be rejected and audit-logged with actor and timestamp

### Validation completeness

**71. Evaluation-policy fields forbidden on non-usage rows**
- **Given** a `flat` non-usage or `per_unit` price row
- **When** `tierAggregationWindow` or `billingGranularity` is present
- **Then** publish MUST fail with a field-level error
- **And** the aggregate fail-closed validation (AC #26) MUST include this condition

**72. Region taxonomy validation**
- **Given** a price row with a `region` value
- **When** the row is saved
- **Then** `region` MUST be a member of the tenant's configured region taxonomy
- **And** an unknown region MUST fail validation before publish

### Grandfathering and concurrency

**73. Grandfathering time-bound at renewal**
- **Given** a grandfathered price row with an optional `grandfatherUntil` (UTC)
- **When** a bound subscription renews on/after `grandfatherUntil`
- **Then** the published read model MUST signal that the subscription is no longer eligible for the grandfathered row
- **And** Subscriptions MUST re-bind it to the current eligible row at renewal (catalog publishes the bound; Subscriptions executes); a null `grandfatherUntil` means indefinite

**74. Concurrency precedence between bulk and interactive edits**
- **Given** an in-flight bulk price import holding per-row optimistic locks (AC #39)
- **When** a concurrent interactive plan-level edit (AC #37) targets a row under bulk lock
- **Then** the interactive edit MUST fail with a conflict naming the bulk operation
- **And** the system MUST NOT silently overwrite either change

### Scope-key and tax validation

**75. Brand taxonomy validation (brand-scoped `PriceList`)**
- **Given** a **brand-scoped `PriceList`** row with a `brand` value (`brand` is a `PriceList` scope, **not** a price-row axis — see §2.2)
- **When** the row is saved
- **Then** `brand` MUST be a member of the tenant's configured brand taxonomy
- **And** an unknown/invalid brand MUST fail validation before publish (mirrors region validation, AC #72)

**76. Tax-exclusive without tax category**
- **Given** a price row with `taxInclusive=false` for a region with no configured `taxCategory`
- **When** publish is requested
- **Then** publication MUST warn or fail per the **tenant tax-display policy** (default fail-closed), symmetric with AC #19
- **And** the behaviour MUST be governed by the **same** tenant tax-display policy object (no separate undefined path)

### Lifecycle and structural integrity

**77. Published price rows are never deleted (supersede/retire only)**
- **Given** a **published** price row on the canonical scope key `(planId, currency, region, priceList, phase, priceEligibility, chargeKind)` — **whether or not** subscriptions are currently bound to it
- **When** removal/deletion of the row is attempted
- **Then** the system MUST reject the deletion — published rows are **append-only history** (AC #32, #97); there is **no deletion event** to fan out to warmed caches, and quotes, previews, exports, and `PriceList` adjustments may reference the row
- **And** the operator MUST use supersession + grandfathering (or retirement + migration) instead, preserving the frozen snapshot for bound subscriptions; **deletion is available only for `draft` rows that were never published**

**78. Future-gap coverage across scheduled windows**
- **Given** a plan with two or more scheduled `PriceWindow` rows for one scope key
- **When** publish runs
- **Then** validation MUST reject any uncovered time interval (gap) between the end of one active/scheduled window and the start of the next for billable periods
- **And** MUST direct the operator to close the gap per the effective-dating price-windows use case (extends the publish-time coverage check, AC #24)

**79. Add-on dependency acyclicity**
- **Given** add-on rules with inter-add-on dependencies
- **When** publish runs
- **Then** the system MUST reject any dependency cycle among add-ons (analogous to phase acyclicity, AC #54)
- **And** MUST report the cycle path

### Currency binding and lifecycle integrity

**80. Invoice currency binding**
- **Given** a subscription activating against a plan with multiple `(currency, region)` price rows
- **When** the subscription is created
- **Then** it MUST bind to exactly one `(currency, region)` row, and all recurring, usage, and add-on lines for that subscription MUST resolve in the bound currency
- **And** publish/preview MUST reject the **enumerated** configurations that would force mixed-currency lines onto a single invoice: (i) an add-on / override / required-add-on row lacking a row in a currency the base plan publishes; (ii) a `sum_of_parts` bundle whose component rows do not cover every currency the bundle sells; (iii) an `own_price` bundle without a matching-currency component set. (currency selection at activation is owned by Subscriptions; the single-currency-per-invoice invariant is enforced here)

**81. Clone field semantics**
- **Given** a clone of a source Plan (AC #38)
- **When** the new draft is created
- **Then** `priceEligibility` and `grandfatherUntil` MUST reset to defaults (not copied), and contract locks MUST NOT be copied
- **And** `discountRef` MUST be copied only if it still resolves to a registered instrument (else dropped with an operator notice); the clone MUST record `clonedFrom` and MUST NOT bind to any source subscription

**82. Upstream SKU retirement coherence**
- **Given** a registry SKU referenced by draft or published plans
- **When** the registry retires/unpublishes that SKU
- **Then** new plan publishes against it MUST be blocked, affected drafts MUST be surfaced to operators, and posted snapshots and active-subscription bindings MUST NOT be mutated
- **And** the catalog MUST emit a signal enabling operators to remediate affected drafts (contract coordinated with the registry)

### Authoring guardrails and discount GA gate

**83. Rev-share residual guardrails**
- **Given** a bundle with rev-share where a non-platform party is nominated primary (AC #23)
- **When** the rounding residual is absorbed
- **Then** no party's resolved share may be < 0%, and the residual MUST be absorbed per `(bundle, vendor SKU)` independently within <= 0.01% tolerance
- **And** publish MUST fail if reconciliation would force any negative share

**84. `customEveryN Days` anchor compatibility**
- **Given** a recurring plan with `frequency=customEveryN Days(n)` (AC #45)
- **When** `billingAnchorPolicy` is `calendar_month` or `fixed_day(d)`
- **Then** publish MUST fail — a `Days`-based custom cycle MUST anchor on `subscription_start`
- **And** `n` MUST NOT exceed the configured custom-interval cap (tied to the size-limit NFR, AC #104)

**85. `PriceList` adjustment tax basis**
- **Given** a `PriceList` markup/discount/fixed adjustment authored against a base price row (AC #55)
- **When** the base row is `taxInclusive=true`
- **Then** the adjustment row MUST declare the basis it applies to (inclusive vs exclusive) or explicitly delegate the basis to Tariffs
- **And** publish MUST reject a `PriceList` adjustment that leaves the basis ambiguous

**86. Refund/cancellation credit basis (conditional)**
- **Given** a recurring price row eligible for cancellation credit — **conditional: this AC activates only on the Finance day-1 decision; otherwise the field remains Future scope**
- **When** published
- **Then** the read model MUST expose a `cancellationCreditBasis` — a **closed** enum (`daily` \| `none`) frozen in `pricingSnapshotRef`, with credit execution owned by Billing/Payments (extending the enum requires a versioned contract change, not an open-ended value set)
- **And** absence MUST mean "no cancellation credit" (fail-safe), **not** undefined

**87. Discount-path availability at sellable-GA**
- **Given** a sellable-GA readiness review
- **When** a Plan is assessed for sale with discounts
- **Then** at least one supported discount path MUST be available (approved Promotions PRD, typed credit row, or `discountRef` in committed scope)
- **And** absent all paths, the Plan MAY publish but MUST be flagged **not-sellable-GA** (per the §13 External dependencies GA gate)
- **And** `discountRef` is **committed to launch scope** (decided 2026-07-04) — the discount path is satisfied at MVP; a full Promotions PRD remains Future

### Package and prepaid pricing

**88. Package (block) pricing**
- **Given** a usage price row with `modelKind=package`, `packageSize`, and `packagePrice`
- **When** the plan is published
- **Then** the read model MUST expose `packageSize` and `packagePrice` with no tier-band fields
- **And** Tariffs MUST bill by rounding usage **up** to whole blocks (blocks = ceil(used / `packageSize`), charge = blocks × `packagePrice`); catalog MUST NOT compute the charge
- **And** this repeating-block formula MUST be reconciled in §17.2 and gated on a joint golden fixture (AC #60) — it is **distinct** from Tariffs' Volume Variant B (per-tier flat fee)

**89. Package pricing validation**
- **Given** a `modelKind=package` price row
- **When** `packageSize <= 0`, `packagePrice < 0`, or any tier-band field is present
- **Then** publish MUST fail with a field-level error
- **And** the aggregate fail-closed validation (AC #26) MUST include these conditions

**90. Prepaid credit grant definition**
- **Given** a plan declaring a prepaid credit grant (`grantAmount`, `creditUnit`, `price`, `expiryPolicy`, `autoRechargeAllowed`)
- **When** the plan is published
- **Then** the grant fields MUST persist and be frozen into `pricingSnapshotRef`
- **And** the grant `price` MUST be scoped per `(currency, region)` like price rows; an unscoped grant price MUST fail publish for a plan selling in multiple `(currency, region)`, and grant-price changes are subject to the same approval-threshold / material-change policy (AC #28, #64)
- **And** the catalog MUST NOT track balance or compute drawdown (owned by Billing/Rating)

**91. Prepaid grant expiry validation**
- **Given** a prepaid credit grant
- **When** `expiryPolicy` is unset or set to `days(N)` with N <= 0
- **Then** publish MUST fail (no implicit "never expires")
- **And** only `never` and `days(N>0)` are valid values

**92. Prepaid grant credit-unit integrity**
- **Given** a prepaid credit grant whose `creditUnit` is a `meteringUnit`
- **When** that `meteringUnit` is not published
- **Then** publish MUST fail
- **And** the catalog MUST NOT persist or expose any prepaid balance (balance ledger owned by Billing/Rating)

### Non-Functional Requirements (Show-Stoppers)

**93. Plan mutation latency**
- **Given** authorized operators
- **When** plan create or update is submitted
- **Then** end-to-end persistence and validation MUST complete within 2 seconds at p95

**94. Price field validation latency**
- **Given** a single price row save
- **When** currency/region/tier validation runs
- **Then** validation MUST complete within 200ms at p95

**95. Publish propagation**
- **Given** a committed `CatalogVersionPublished` event (the point at which a batched publish becomes addressable)
- **When** Rating cache warming runs
- **Then** plan definitions MUST be visible within 5 seconds at p95 of `CatalogVersionPublished`
- **And** the registry MUST bound the delay from `PlanPublished` (pending) to `CatalogVersionPublished` (committed) by a stated **max batching-delay SLO** (ratified with the registry — see §15 Open Questions)

**96. Plan read latency**
- **Given** catalog browse or plan detail read
- **When** served from read model within tenant partition
- **Then** p95 latency MUST be under 100ms

**97. Audit completeness and tamper-evidence**
- **Given** any plan or price mutation
- **When** the operation completes
- **Then** the system MUST record actor, timestamp, before/after version, and approval trail
- **And** MUST retain price history for **>= 7 years**, with the retention period **tenant/jurisdiction-configurable**
- **And** where multiple jurisdictional minimums apply, retention MUST be the **maximum** applicable minimum per the data row's residency jurisdiction(s)
- **And** audit and price-history records MUST be stored **append-only / tamper-evident** (WORM or hash-chained) so prior versions cannot be mutated or deleted within the retention window
- **And** where a jurisdiction imposes a retention *maximum* (storage-limitation regime) that conflicts with the max-minimum rule, Legal MUST resolve the conflict per the row's residency jurisdiction (see §15 Open Questions)

**98. Event propagation**
- **Given** `PlanCreated` or `PlanUpdated`
- **When** emitted after mutation
- **Then** downstream consumers MUST receive event within 5 seconds at p95
- **And** events MUST include correlation/idempotency keys per manifest governance
- **And** consumers MUST be able to dedupe by idempotency key (at-least-once delivery assumed; duplicate redelivery MUST be safe)

**99. Multi-currency scale**
- **Given** a Plan with many currency rows
- **When** stored and read
- **Then** the system MUST support **at least 20 currencies per plan as a guaranteed floor** (tested at 20; not merely a benchmark point)
- **And** the read SLO (p95 < 100ms, AC #96) MUST hold **at the 20-currency floor**, not only at nominal load

**100. Mutation API idempotency**
- **Given** a plan or price create/update call carrying a client idempotency key
- **When** the same key is retried
- **Then** the system MUST return the original result and MUST NOT create a duplicate draft plan or price row
- **And** the key MUST be honoured for a defined retention window (TTL); a key reused after the TTL MAY be treated as a new request, and the TTL MUST be documented in the API contract (Design)
- **And** an idempotency-key **replay during an active bulk lock** (AC #74) MUST return the original completed result regardless of current lock state (it is a read of a completed op); a **new** key targeting a locked row follows AC #74

**101. Mass repricing performance and idempotency**
- **Given** a mass price adjustment over N rows (e.g. an annual increase across many plans)
- **When** executed or re-executed after a partial failure
- **Then** the operation MUST be idempotent (re-run-safe) and MUST emit deduplicated events
- **And** the throughput SLO MUST be back-calculated from the tenant worst-case row count (plans × currencies × regions) against an agreed maintenance window (provisional >= 50 rows/sec; Design MUST confirm against the worst case, not adopt the provisional figure blindly)

**102. Data residency for price and audit data**
- **Given** a tenant with a jurisdiction-bound residency requirement
- **When** price and audit/history data are stored and replicated
- **Then** the system MUST keep that data within the configured jurisdiction's residency boundary and MUST NOT replicate a row outside **any** of its residency jurisdictions' boundaries
- **And** residency MUST be consistent with the jurisdiction-configurable retention in the audit-completeness NFR (AC #97)

**103. Read-model availability and disaster recovery**
- **Given** Rating's hard real-time dependency on the published read model
- **When** the read model serves rating/preview traffic
- **Then** it MUST meet a stated availability SLO (target >= 99.9% per tenant partition) and a disaster-recovery **RPO <= 5 min** / **RTO <= 30 min** (provisional defaults; Architecture MUST ratify before Design — no bare placeholders may ship)
- **And** during a read-model outage, charge resolution MUST **NOT resolve from an unavailable or partially-warmed read model** and MUST fail closed; **frozen `pricingSnapshotRef` resolution is unaffected** (rating against an intentionally frozen snapshot is the core mechanism, not "stale" rating), consistent with AC #62

**104. Plan and tier size limits**
- **Given** a Plan with many tier bands and/or price rows
- **When** authored or published
- **Then** the system SHOULD enforce configurable soft caps on tier-band count per row and price-row count per plan, emitting a publish warning above the cap (provisional defaults: <= 100 tier bands per row, <= 500 price rows per plan — testable defaults, tenant-configurable; Design may tune)
- **And** the caps MUST protect the read SLO (AC #96) and mass-repricing SLO (AC #101) from pathological plans

### Commercial-shape and lifecycle completeness (review follow-ups)

**105. Recurring billing timing (`billingTiming`)**
- **Given** a recurring or hybrid Plan submitted for publish
- **When** validation runs
- **Then** every recurring price row MUST carry `billingTiming` (`in_advance` \| `in_arrears`), frozen in `pricingSnapshotRef`; usage rows are implicitly `in_arrears`
- **And** publish MUST fail if a recurring row omits `billingTiming`; a hybrid plan MAY combine an `in_advance` base row with `in_arrears` usage rows (Billing derives its deferral policy from this — Billing PRD)

**106. Optional one-time setup/activation charge on recurring/hybrid plans**
- **Given** a recurring or hybrid Plan with an optional one-time setup price row on the same `planId`
- **When** the Plan is published
- **Then** the read model MUST expose the setup row as a **first-class** plan price row (in approvals, `pricingSnapshotRef`, and preview), charged **once at activation**
- **And** publish MUST validate it as one-time (no recurrence, no `billingTiming`/tier fields) — it MUST NOT require a synthetic add-on SKU

**107. Purchase gated on an active window (sellability)**
- **Given** a published Plan whose bound canonical scope key has only a **scheduled** (not-yet-active) `PriceWindow` at time `t`
- **When** subscription/purchase creation is attempted at `t`
- **Then** creation MUST be blocked (joint rule with Subscriptions) — a plan MUST NOT be sold before an active window covers the bound scope key, preventing a first rating that resolves no window (fails closed)
- **And** optional plan-level `availableFrom`/`availableTo` (the unified purchasability-dating pair for **all** billing cycles), when set, MUST be validated against window coverage at publish; deferred publish ("publish at T") is out of launch scope

**108. Plan-change contract published**
- **Given** a Plan that participates in self-service plan changes
- **When** the Plan is published
- **Then** the read model MUST publish `allowedChangeTargets` (explicit target `planId`s or a rule) and a `comparabilityRank` (or an authoritative published `PlanTier` ordering) so Subscriptions can classify upgrade/downgrade/switch and constrain targets
- **And** absence of `allowedChangeTargets` MUST mean **no self-service change** (fail-safe), not any-to-any; enforcement remains in Subscriptions

**109. Customer-group segment pricing**
- **Given** an operator-defined customer group (BSS-owned taxonomy) and a `customerGroup`-scoped `PriceList` with an adjustment (`markup`/`discount`/`fixed`) per `(group, region)`
- **When** the plan is published and a payer's effective-dated group membership is resolved via `payerTenantId`
- **Then** the read model MUST expose the `customerGroup` `PriceList` and the group taxonomy; Tariffs resolves the payer's group and applies the overlay (§17.7); the **resolved group** MUST be frozen in `pricingSnapshotRef`
- **And** a membership change is **renewal-aligned by default** (resolved group pinned in the subscription snapshot until renewal; immediate re-resolution is an explicit material change), a group discount/move affecting many payers is a **material change** (two-person rule, AC #28), and all membership changes MUST be audited
- **And** entirely different **tier structures** per group are **out of launch scope** (use separate plans + group-scoped plan eligibility — Future)

## 13. Dependencies

| Dependency | Description | Criticality |
|------------|-------------|-------------|
| Catalog registry (Product & SKU) | Published `skuId`, `bundle` SKU type, `meteringUnit` declaration, `PlanTier` value/taxonomy, tax/GL codes, and `CatalogVersion` (sole incrementer); increment taxonomy **Proposed** | `p1` |
| PriceWindow (effective-dating use case) | Window scheduling, UTC activation jobs, `PriceWindowScheduled`/`Activated`/`Cancelled`/`Expired` mechanics; this PRD attaches windows and validates coverage | `p1` |
| Tariffs / PLAL | Consumes the read model (model kind, tiers, evaluation policy, reserved rate, derived-meter definition, `customerGroup` overlays); evaluates formulas/overlays/FX; composes `pricingSnapshotRef` | `p1` |
| Subscriptions | Executes proration math, plan-change classification, trial runtime, entitlement enforcement, `PlanLink` migration, sellability checks from published inputs; proration seam is a GA gate | `p1` |
| Rating | Consumes events + warmed read models; resolves deterministic inputs; owns Usage -> RatedCharge orchestration | `p1` |
| Billing / Payments | Consumes descriptors via `CatalogVersion`; derives deferral policy from `billingTiming`; owns refunds/credits and PSP/ERP posting; minimum descriptor field set TBD | `p1` |
| Tax Engine | Tax-scheme determination + `region` -> jurisdiction mapping; **confirmed post-MVP** (ETA ~8 months) — MVP is tax-exclusive; tax-inclusive plans GA-gated | `p1` |
| Contracts & Agreements | Contract locks (exclude locked subscriptions from migration); negotiated RI-style reservation rates | `p1` |
| Promotions / Coupons | Coupon/discount authoring + evaluation (dedicated PRD **TBD — does not yet exist**); `discountRef` resolves to a registered external instrument | `p2` |
| Marketplace | Consumes bundle rev-share rules for fee accrual | `p2` |
| Prepaid balance ledger + drawdown + auto-recharge | Billing / Rating; **TBD** — prepaid grants can be defined but not sold/consumed until balance execution exists | `p2` |
| BSS Architecture Manifest | §4.1 Catalog, §4.2 Rating, §4.3 Subscriptions, §2.1.3 identities, §4.8 Marketplace | `p1` |

> **Conformance fixtures before code**: the jointly-owned golden fixtures (tier-boundary per AC #60, proration per AC #61) MUST be stood up and version-controlled in a shared repo **before** implementation; publish-contract sign-off is gated on green fixtures. Every GA-gate dependency MUST be tracked on the program board with a named owner and target date.

## 14. Assumptions

- NFR provisional targets (read-model availability/DR RPO/RTO, mass-repricing worst-case throughput, plan/tier size caps, idempotency-key TTL) are working assumptions and MUST be ratified before Design lock; no bare placeholders may ship.
- The catalog registry is SoR for Product/SKU/Category/Attribute/`CatalogVersion`, the `bundle` SKU type, the metering-unit declaration, and the `PlanTier` taxonomy; this PRD composes **published** units and **freezes** plan/price/descriptor content into the `CatalogVersion` the registry commits.
- `CatalogVersion` uses a **batched/discretionary** publish model: `PlanPublished` carries a pending ref and `pricingSnapshotRef` pins the committed batch version; the exact increment-trigger taxonomy and the max batching-delay SLO value remain to be confirmed with the registry.
- Tax Engine is confirmed **post-MVP**; MVP sells **tax-exclusive** and `taxInclusive=true` plans are authorable but flagged **not-sellable-GA** until Tax Engine GA.
- `discountRef` is committed as the **day-1 discount hook** (referential-integrity only); a full Promotions PRD remains the durable owner (Future).
- Subscriptions executes proration/runtime/`PlanLink`; Tariffs evaluates formulas, overlays, and FX; Billing posts invoices and derives deferral policy from `billingTiming`; the catalog computes **no** monetary charges.
- PriceWindow scheduling/activation is owned by the effective-dating price-windows use case; this PRD requires linkage and publish-time coverage only.
- Prepaid credit **balance execution** (ledger, drawdown, zero cut-off, auto-recharge) is owned by Billing/Rating; the catalog defines the grant only.

## 15. Open Questions

> Decisions already resolved for this PRD are recorded inline (Glossary, §17 appendices, Acceptance Criteria). This table lists only items still requiring an external decision or sign-off.

| **Question** | **Status / Owner** | **Date Answered** |
|--------------|--------------------|-------------------|
| Day-1 discount: ratify a Promotions PRD (owner + date) **or** commit `discountRef` into scope? | **Answered — `discountRef` committed into launch Scope** (day-1 discount hook; a full Promotions PRD remains the durable owner, Future). Owner: Program + GTM. | 2026-07-04 |
| Tax Engine PRD exists (scheme determination + `region`->jurisdiction mapping)? | **Answered — confirmed but post-MVP** (roadmap P1 right after MVP, ETA ~8 months). MVP is **tax-exclusive**; `taxInclusive=true` is **authorable but a GA gate** (AC #18/#19). Owner: Architecture/Tax. | 2026-07-04 |
| `CatalogVersion` increment trigger taxonomy (per-publish vs **batched/discretionary**; mass-repricing coalescing; **max batching-delay SLO** per AC #95); registry as sole incrementer? | **Answered (model) — batched/discretionary** catalog publish adopted (pending-ref on `PlanPublished`; `pricingSnapshotRef` pins the batch). **Still open with Registry:** exact trigger taxonomy + the max batching-delay SLO **value** (AC #63, #95, #101). Owner: Registry. | 2026-07-04 (model) |
| Upstream SKU retirement/unpublish joint contract while plans reference it? | **Open** — catalog side specified (AC #82); Registry to confirm the joint contract. Owner: Registry. | — |
| Minimum Billing/ERP descriptor field set + PSP/ERP posting contract beyond `glCode` + events? | **Open** — AC #53 requires sufficiency; field list still TBD. Owner: Billing/Payments. | — |
| Cancellation/refund credits required at launch? | **Deferred** — if yes, conditional AC #86 (`cancellationCreditBasis`, fail-safe = no credit) activates and moves to Scope. Owner: Finance + Billing. | — |
| Cross-boundary (currency/region/frequency) cancel+new limitation — written sign-off + customer-facing constraint entry? | **Open** (AC #66). Owners: Subscriptions, Finance, GTM. | — |
| Two-dimensional (seats × usage) single-line pricing needed at launch? | **Open** — if yes it leaves Future scope (workaround = two rows). Owner: Product. | — |
| Freemium: per-row `$0` vs a first-class structural flag at launch? | **Open** — if reporting cannot distinguish freemium from a `$0` promo band, the flag moves forward. Owner: Product + Analytics. | — |
| Provisional NFR values to ratify before Design (no bare placeholders may ship): read-model availability/DR RPO/RTO (AC #103), mass-repricing worst-case throughput (AC #101), plan/tier size caps (AC #104), idempotency-key TTL (AC #100)? | **Open**. Owners: Architecture, Ops, Finance. | — |
| Retention "max applicable minimum" vs jurisdictions imposing a retention *maximum* (storage-limitation)? | **Open** — Legal to resolve per residency jurisdiction (AC #97). Owner: Legal. | — |
| Rate-limiting / per-tenant mutation quotas (abuse control)? | **Deferred** — out of this PRD's scope but MUST land as a platform/gateway NFR. Owner: Engineering/Platform. | — |
| Minimum customer notice for enforced migration (business policy)? | **Open** — platform SHOULD support configurable lead time; default 60-90 days. Owner: Finance/GTM. | — |
| SSP / ASC 606 tags on plan publish? | **Deferred** to Future scope / Finance workshop. Owner: Finance. | — |
| Consolidate the effective-dating price-windows use case into a standalone PRD? | **Open** — linkage requirements live here; scheduling stays in the use case until merge. Owner: Architecture. | — |
| Volume **Variant B** (per-tier block fee) — needed at launch or dropped from Tariffs? (F-39) | **Answered — dropped from Tariffs** (not authorable from the catalog; catalog `volume` = Variant A only). Owner: Product + Tariffs. | 2026-07-04 |
| Self-service **reserved-capacity rate** a launch/migration requirement (§17.7)? (F-42) | **Answered — yes, in launch Scope** (predecessor parity). Tariffs sources the reserved rate from the catalog snapshot; negotiated RI-style rates stay in Contracts. Owner: Product + Tariffs + Contracts. | 2026-07-04 |
| Peak / period-end billing — authorable **`aggregationFunction`** (`sum`\|`max`\|`last`\|`unique`) / capacity-peak kind a launch case? (F-40) | **Answered — no** (not at launch); usage `Q` is always **window-sum** (Glossary); `aggregationFunction`/peak stays Future. Owner: Product + Rating. | 2026-07-04 |
| First-class **`includedAllowance`** (+ `rolloverPolicy`) vs the `$0`-first-band representation at launch? (F-32) | **Answered — no** (keep the `$0`-first-band; display/reporting limitation recorded in Glossary); `includedAllowance` stays Future. Owner: Product + Analytics. | 2026-07-04 |
| Tariffs to add a **partner-facing effective-price preview** (with hierarchy) so the migrated use-case promise has an owner? (F-34) | **Open** — AC #41 publishes base list price only. Owner: Tariffs + GTM. | — |
| **Draft/sandbox bill simulation** extension (simulate a draft plan via downstream evaluation in sandbox, keeping the no-charge-computation boundary)? (F-36) | **Open** — Tariffs simulation is published-state only; consumers MUST NOT read draft (AC #50). Owner: Tariffs + Product. | — |
| **Localization** owner for Plan-owned display fields (extend the registry localization mechanism to Plan/phase/descriptor content; Presentation renders)? (F-37) | **Open** — descriptors are single-language today. Owner: Architecture + Presentation. | — |
| **Customer-group** pricing (per-`(group, region)`) a launch/migration requirement? (F-41, reopened as F-88) | **Answered — yes, in launch Scope (MAJOR)** — adjustment-based `customerGroup` `PriceList` axis (resolved via `payerTenantId`; BSS-owned effective-dated audited membership; §17.7 + AC #109). Different per-group tier *structures* stay Future. Owner: Product. | 2026-07-04 |
| Self-service **term / auto-renew** metadata home (optional `termLength`/`autoRenew` on the Plan)? (F-35) | **Answered — no** (not at launch); explicit boundary added (Out of Scope) — term/auto-renew is not a catalog concern at MVP. Owner: Product + Subscriptions + Contracts + Billing. | 2026-07-04 |
| Unify purchasability dating into one field pair, and decide whether **deferred publish** ("publish at T") is a launch need? | **Answered — unify: yes** (one `availableFrom`/`availableTo` pair for **all** billing cycles); **deferred publish: no** (out of launch scope). Owner: Product + Architecture. | 2026-07-04 |
| Document owner + target release? | **Open** — assign a document owner and target-release milestone before Design. Owner: Program/Architecture. | — |

## 16. Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| Tax Engine slips beyond its post-MVP ETA | Tax-inclusive plans stay not-sellable-GA; `region`->jurisdiction mapping deferred | MVP sells tax-exclusive; `taxInclusive=true` authorable but GA-gated (AC #18/#19); track Tax Engine GA on the program board |
| `CatalogVersion` increment taxonomy / batching SLO unconfirmed by Registry | `pricingSnapshotRef` determinism and the AC #95 propagation SLO undefined | Batched/discretionary model adopted now (AC #63); block publish-contract sign-off until Registry confirms taxonomy + SLO value |
| Proration / plan-change enum drift across Subscriptions and Tariffs | Wrong credits on up/down-grade; divergent proration | Canonical `prorationBasis` enum owned here and adopted verbatim; shared golden fixtures (AC #61) before code |
| Conformance fixtures (tier-boundary, package, per_unit, proration) not stood up before implementation | Off-by-one / model-kind mispricing reaches production | Publish blocked for any `modelKind` lacking a joint golden fixture (AC #60); fixtures version-controlled before code |
| Promotions PRD does not yet exist | No coupon authoring/evaluation owner at launch | `discountRef` committed as the day-1 hook (referential-integrity only, AC #69); full Promotions PRD remains the durable owner |
| Prepaid balance execution (ledger/drawdown/auto-recharge) absent | Prepaid grants definable but not sellable/consumable | Catalog defines the grant only (frozen in snapshot); GA-gate the sellable path on Billing/Rating balance execution |
| Read-model availability/DR numbers provisional | Rating hot-path resilience unproven | Fail-closed on read-model outage (AC #103); Architecture ratifies availability/RPO/RTO before Design |
| Upstream SKU retirement joint contract open | Draft/published plans may reference a retired SKU | Catalog side specified (AC #82); confirm the joint remediation contract with the Registry |

## 17. Reference Materials

| **Material** | **Link / path** | **Comments** |
|--------------|-----------------|--------------|
| BSS Architecture Manifest | `docs/bss/manifest/vz-arch-manifest-bss-only.md` | §4.1 Catalog; §4.2 Rating; §4.3 Subscriptions |
| Product Catalog & Marketplace PRD | `docs/bss/prd/PRD-product-catalog-marketplace-202601120119/PRD-product-catalog-marketplace-202601120119.md` | Parent catalog; SKU lifecycle |
| Product & SKU Management PRD | `docs/bss/prd/PRD-product-sku-management-202606101924/PRD-product-sku-management-202606101924.md` | Catalog registry (SoR for Product/SKU/Category/Attribute/`CatalogVersion`, `bundle` SKU type, metering-unit declaration, `PlanTier` taxonomy) this PRD builds on |
| Tariffs — Commercial Pricing Logic PRD | `docs/bss/prd/PRD-tariffs-pricing-logic-202604011200/PRD-tariffs-pricing-logic-202604011200.md` | Formulas, hierarchy, `pricingSnapshotRef`; promotions/FX boundary |
| Rating Engine PRD | `docs/bss/prd/PRD-rating-engine-202604031200/PRD-rating-engine-202604031200.md` | Usage -> billable items (draft) |
| Subscriptions & Entitlements PRD | `docs/bss/prd/PRD-subscriptions-entitlements-202601120119/PRD-subscriptions-entitlements-202601120119.md` | `PlanLink`, migration execution |
| Contracts & Agreements PRD | `docs/bss/prd/PRD-contracts-agreements-202601120119/PRD-contracts-agreements-202601120119.md` | Contract locks |
| Metering & Pricing Module PRD | `docs/bss/prd/PRD-metering-pricing-module-202601120119/PRD-metering-pricing-module-202601120119.md` | Usage collection |
| Billing Ledger & Balances PRD | `docs/bss/prd/PRD-billing-ledger-balances-202604041200/PRD-billing-ledger-balances-202604041200.md` | Posted invoice immutability |
| Plan & Price Modeling use case | `docs/bss/prd/PRD-product-catalog-marketplace-202601120119/UC-plan-price-modeling-202601121200.md` | Scenario reference; superseded |
| Effective Dating & Price Windows use case | `docs/bss/prd/PRD-product-catalog-marketplace-202601120119/UC-effective-dating-price-windows-202601121200.md` | Window scheduling authoritative |
| Project glossary | `docs/project-glossary.md` | Canonical terms |
| Trace chain | `AGENTS.md` (repository root) | Manifest -> PRD -> ADR -> Design -> Stories |

### 17.1 Supported Billing Cycles and Price Structure Kinds (catalog)

**Supported billing cycles**

| **Billing cycle** | **Intent** | **Catalog requirements** |
|-------------------|------------|---------------------------|
| **One-time** | Single purchase | One-time base price; optional quantity min/max; optional availability window; MUST NOT attach recurring-only add-ons |
| **Recurring** | Subscription fee (`monthly`, `quarterly`, `semiannual`, `annual`, or `customEveryN{Days\|Months}(n)`) | Base price row(s) per currency/region; links to `PriceWindow`; supports plan phases and `per_unit` (per-seat) pricing; `billingTiming` (`in_advance`\|`in_arrears`) REQUIRED at publish; MAY declare an optional one-time setup row; `frequency` and any custom interval `n` persisted as metadata (publish MUST reject non-positive `n`) |
| **Usage-based** | Charge by metered consumption | Requires parent SKU `meteringUnit`; tier bands or flat unit price; MUST set `billingGranularity` on **all** usage rows (flat or tiered); MUST set `tierAggregationWindow` **when tiered** |
| **Hybrid** | Base fee + variable usage | Same `planId` MUST declare both recurring and usage price components; publish validation MUST reject hybrid without both parts; MAY additionally declare an optional one-time setup/activation row |

**Supported price structure kinds (catalog flags)**

| **Kind** | **When used** | **Catalog MUST persist** |
|----------|---------------|---------------------------|
| **Flat** | Fixed unit or period price | `modelKind=flat`; single amount or unit price |
| **Per-unit (per-seat)** | Recurring unit price x external quantity (seats/users) | `modelKind=per_unit`; unit price + `quantitySource` (`subscription_seat_count` \| `manual`); quantity resolved by Subscriptions/operator, **never metered here** |
| **Graduated** | Marginal tier pricing | `modelKind=graduated`; ordered non-overlapping bands |
| **Volume** | Single rate on total quantity | `modelKind=volume`; ordered non-overlapping bands |
| **Package (block)** | Usage billed in whole blocks (e.g. 100 units for $80; 150 used -> 2 blocks -> $160) | `modelKind=package`; `packageSize` (> 0) + `packagePrice` (>= 0); tier-band fields absent; round-up math in Tariffs |
| **Tiered (unspecified)** | — | MUST NOT publish; operator MUST choose graduated or volume |

Mathematical formulas belong in Tariffs Design; catalog MUST NOT compute charges.

### 17.2 Model Kind / Tariffs Formula Mapping (conformance)

The catalog `modelKind` enum and the Tariffs formula matrix MUST reconcile one-to-one; publish of any `modelKind` lacking a **joint golden conformance fixture** MUST be blocked (AC #60). This table is the **single** kind-to-formula source of truth — Tariffs and this PRD MUST NOT diverge from it:

| **Catalog `modelKind`** | **Tariffs formula** | **Status / joint fixture** |
|-------------------------|---------------------|-----------------------------|
| `flat` | Flat | Exists |
| `graduated` | Tiered (graduated) | Exists (AC #60 boundary fixture) |
| `volume` | Volume — **Variant A** (single rate on total `Q` within `tierAggregationWindow`) | Exists; catalog `volume` maps to **Variant A only** |
| `package` | **Repeating-block** (`blocks = ceil(used / packageSize)`, charge `= blocks x packagePrice`) | **Present in Tariffs**; distinct from Volume Variant B (stair-step per-tier flat fee); joint golden fixture + Tariffs AC pending (AC #60/#88) |
| `per_unit` (per-seat) | **External-quantity** (`unitPrice x quantity` from `quantitySource`) | **Present in Tariffs**; joint golden fixture + Tariffs AC pending (AC #60) |
| `usage` row + `reservedRate`/`reservationFlavor` (reservation variant — **not** a distinct `modelKind`) | **Reserved-rate + on-demand** (reserved rate on matched/allocated quantity, on-demand rate on the remainder; `capacity` flavor bills the allocation regardless of usage) | **Evaluation exists** in Tariffs (step 6); the change needed is **sourcing** — Tariffs MUST take the **self-service** reserved rate from the catalog row (snapshot-frozen) — Tariffs change + joint fixture (AC #60) |

Tariffs' **Volume Variant B** (per-tier block fee) is **dropped** (decided 2026-07-04) — it was not authorable here (catalog tier bands carry unit prices only, no per-band flat-fee field); catalog `volume` maps to **Variant A** only.

### 17.3 Plan Composition Rules (normative)

| **Rule** | **Statement** |
|----------|---------------|
| Parent SKU | Plan MUST reference a **published** SKU; draft SKU MUST block plan publish |
| PlanTier | Every Plan MUST declare `PlanTier` before publish (optional at draft); the taxonomy and SKU-level value are owned by the registry. Publish MUST validate that the Plan's `PlanTier` **equals its parent SKU's `PlanTier`** unless an **explicit, audited override** is declared (default: equal) — no silent divergence (AC #7) |
| Meter injective | Each usage plan **revision** MUST map exactly one `meteringUnit` (per-row injectivity: one priced line per `(meter, dimensionKey)`); ambiguous mapping MUST fail publish. **Multi-meter offerings** are modeled either as a **derived (composite) meter** (one output unit) or as **separate single-meter SKUs composed via bundle/add-ons** (both in launch Scope) |
| Add-on compatibility | Add-on SKUs MUST be published and compatible with base SKU; conflicts MUST fail publish |
| Bundle components | All `includedSkuIds` MUST be published; rev-share MUST sum to 100% per vendor SKU when set |
| Bundle price basis | A Bundle MUST declare its price basis: `sum_of_parts` or `own_price`; the basis and any explicit price MUST be persisted and frozen. For `sum_of_parts` the bundle MUST reference the specific **component `planId`s** (not bare `skuId`s) whose rows are summed, and publish MUST validate that **every** referenced component has a **covering published price row in each `(currency, region)` the bundle sells in** (and matching `frequency` for recurring components). A missing or ambiguous component row MUST fail publish. Rev-share and itemization are independent of the basis |
| Billing descriptors | Publish MUST include complete billing descriptor set per manifest §4.1 |
| Price window coverage | For billable usage at time `t`, an active `PriceWindow` MUST exist for the resolved **canonical scope key** (resolved on the **base** `priceList`) or Tariffs step 2 MUST fail (no silent fallback). Because `priceEligibility` and `chargeKind` are part of the key, a grandfathered row and its successor, and the `recurring`/`usage`/`one_time_setup` components of a hybrid plan, are **distinct keys** that MAY each hold an active window at the same `t` |
| Hybrid completeness | Hybrid plans MUST include at least one recurring and one usage price row |
| One-time setup charge | Recurring and hybrid plans MAY declare an **optional one-time setup/activation price row** on the same `planId`, charged **once at activation** and frozen in `pricingSnapshotRef`. It is a **first-class plan price row** — **not** a synthetic add-on SKU. Publish MUST validate it as one-time (no recurrence, no `billingTiming`/tier fields) |
| Sellability gate | A subscription/purchase MUST NOT be created while **either** no active (not merely *scheduled*) `PriceWindow` covers the bound canonical scope key at `t`, **or** the plan is not yet addressable in a **committed `CatalogVersion`** — a joint rule with Subscriptions. Plans of **any billing cycle** MAY declare optional plan-level `availableFrom`/`availableTo` (validated against window coverage at publish); **deferred publish** is out of launch scope |

### 17.4 Price Validation Rules (catalog)

| **Rule** | **Validation** |
|----------|----------------|
| Amount sign | Price amount MUST be **>= 0**. A `0` amount is valid for free tiers, `trial`/`intro` phases, and the first graduated band. Negative amounts MUST be rejected (typed credit/discount rows are Future scope). |
| Currency | MUST be valid ISO 4217 |
| Currency coverage (no implicit FX) | Catalog MUST NOT perform FX conversion. If no price row exists for a requested `(currency, region)`, preview/publish MUST fail closed; no base-currency fallback unless an explicit `currencyFallbackPolicy` is configured (Future). FX is owned by Tariffs/PLAL. |
| Precision | Amount precision MUST follow the **ISO 4217 minor-unit** for the row's currency (0 for JPY/KRW, 2 default, 3 for BHD/KWD/OMR); publish MUST reject amounts with more precision than the currency's minor unit. A flat 2-decimal cap MUST NOT be assumed. |
| Duplicate scope | MUST NOT allow a duplicate price row on the **canonical scope key** without the supersession workflow. Rows differing only by `priceList`, `phase`, `priceEligibility`, or `chargeKind` (e.g. the `recurring` and `usage` components of a hybrid plan) are **distinct**, not duplicates |
| Tier order | Tier bands MUST have ascending `fromQty`; ranges MUST NOT overlap |
| Tier contiguity | Tier bands MUST be contiguous (no coverage gaps) under the `[fromQty, toQty)` convention; the top band MUST be open-ended (`toQty=null`) unless an explicit maximum is intended. A **closed** top band MUST trigger a **publish-time warning**, and quantity exceeding the closed top MUST **fail closed** at evaluation (Tariffs/Rating — **no** free, **no** clamp); the read model persists the explicit maximum and the fail-closed marker |
| Per-unit quantity source | `modelKind=per_unit` rows MUST persist a `quantitySource` (`subscription_seat_count` \| `manual`); catalog MUST NOT infer or compute the quantity |
| Evaluation-policy placement | `tierAggregationWindow` and `billingGranularity` are **usage-row only**; on `flat` non-usage and `per_unit` rows they MUST be **absent** — presence MUST fail publish |
| Region taxonomy | A price row's `region` MUST be a member of the tenant's configured region taxonomy; an unknown/invalid region MUST fail validation before publish |
| Brand taxonomy | `brand` is a `PriceList` scope value, **not** a price-row field. A **brand-scoped `PriceList`**'s `brand` MUST be a member of the tenant's configured brand taxonomy; an unknown/invalid brand MUST fail validation before publish (AC #75) |
| Tax basis completeness | `taxInclusive=true` without a region tax rate **and** `taxInclusive=false` in a region with no configured `taxCategory` are both governed by the tenant tax-display policy (warn/fail; default fail-closed) |
| Min-quantity floor | A price row's `minQtyThreshold` MUST declare its **floor type** — `purchase` (Subscriptions rejects orders below the floor) or `usage` (Tariffs/Rating treats usage below the floor as ineligible and fails closed to the plan's fallback, **not** silently zero-rated). Publish MUST reject a `minQtyThreshold` with no declared floor type, and MUST warn if it falls inside a non-zero-priced band |
| Package pricing | `modelKind=package` MUST persist `packageSize` > 0 and `packagePrice` >= 0; tier-band fields MUST be absent; publish MUST reject otherwise |
| Prepaid grant | If a plan declares a prepaid credit grant, `expiryPolicy` MUST be set (`never` or `days(N>0)`) and `creditUnit` MUST reference a currency or a **published** `meteringUnit`; catalog MUST NOT persist any balance |
| Tax-inclusive | If `taxInclusive=true`, tax rate MUST exist for region or publish MUST warn/fail per the **tenant tax-display policy** |
| Required add-on | Required add-on MUST have `maxQty >= 1` |

### 17.5 Price-Change Mechanisms and CatalogVersion Increment Contract

**Price-change mechanisms (versioning, windows, supersession)** — four mechanisms describe price change over time; they are **distinct and composable**, not alternatives:

| **Mechanism** | **What it does** | **Artifact** | **Event** |
|---------------|------------------|--------------|-----------|
| Plan **versioning** | Captures a structural/price change as a new immutable revision; prior rows retained as history | New `Price` row(s) + new plan revision | `PlanUpdated` / `PriceCreated` |
| **Supersession** | Replaces the active row for one **canonical scope key** with a newer one | New `Price` row marked superseding; prior row closed | `PriceUpdated` |
| **`PriceWindow`** | Schedules **when** a versioned/superseded row is effective | `PriceWindow` linkage (activation owned by the effective-dating use case) | `PriceWindowScheduled` / `Activated` |
| **Grandfathering cutover** | Atomically **closes the current `all_subscriptions` window by shortening its `effectiveTo` to the cutover** (active windows are **not** cancelled) and creates (a) an `existing_grandfathered` **copy** for pre-cutover subscriptions and (b) the `all_subscriptions` successor, as **one approval unit** so no coverage gap opens | Shorten current window `effectiveTo` + schedule `existing_grandfathered` copy + schedule successor (one atomic unit) | `PriceUpdated` + `PriceWindowScheduled` x2 + `PriceWindowExpired` at cutover; `PriceWindowCancelled` only for *not-yet-active* windows of the old key |

Normative relationship: **supersession is versioning scoped to one canonical scope key** — it MUST create a new immutable `Price` row (never mutate in place) and MUST open/close the corresponding `PriceWindow` rather than overlap it. Supersession operates **within one `priceEligibility` class and one `chargeKind`**. An `existing_grandfathered` row is **immutable in price** — it MUST NOT be superseded; the **only** permitted mutation is **setting or tightening `grandfatherUntil`** (never loosening, never the price), which is a **material change** (AC #28). No mechanism may produce overlapping active `PriceWindow` rows for **one** canonical scope key (manifest invariant, as extended in §2.2).

**CatalogVersion increment contract (with registry)** — `CatalogVersion` is owned by the registry; this PRD **freezes** plan/price/descriptor content into it but does not re-define it:

| **Change class** | **New `CatalogVersion`?** | **Incrementer** |
|------------------|---------------------------|-----------------|
| Price-only edit (amount/window on existing plan) | Yes — content MUST become addressable in a `CatalogVersion` (MAY be **batched**) | Registry, on catalog publish request |
| Structural edit (model kind, tiers, descriptors, composition) | Yes — addressable in a `CatalogVersion` (MAY be batched) | Registry, on catalog publish request |
| Draft-only edits (no publish) | No | — |

On **every** `PlanPublished`, this PRD MUST request that the plan's content become addressable in a `CatalogVersion`; the registry is the **sole** incrementer and **MAY batch** multiple approved publishes into **one** discretionary catalog publish. `PlanPublished` carries a **pending** version reference; the committed `CatalogVersion` is emitted as `CatalogVersionPublished`, and `pricingSnapshotRef` MUST pin that committed version (AC #27, #63). The exact increment-trigger taxonomy and the **max batching-delay SLO** from `PlanPublished` to `CatalogVersionPublished` are confirmed with the registry owner (§15).

### 17.6 Consumer Contracts Detail

**Proration input contract (catalog -> Subscriptions)** — this PRD owns **publishing the inputs**; the proration math, mid-cycle change, and credit application are owned by Subscriptions. Each recurring price row MUST expose the following in the read model, frozen in `pricingSnapshotRef`:

| **Field** | **Requirement** |
|-----------|-----------------|
| `billingAnchorPolicy` | When the billing period anchors (`calendar_month`, `subscription_start`, `fixed_day(d)`); MUST be set on recurring rows; month-end/UTC handling per Glossary |
| `prorationBasis` | `calendar_days_actual` \| `calendar_days_30` \| `by_second` \| `whole_unit` \| `none` (canonical enum per Glossary, adopted verbatim by Tariffs); how partial periods are apportioned |
| `creditOnDowngrade` | Whether a mid-cycle downgrade is eligible for catalog-sanctioned credit (Subscriptions applies it) |

> **Cross-boundary mid-cycle changes (launch scope)**: Mid-cycle plan changes that **cross currency, region, or billing frequency** are **NOT supported at launch** — the proration input contract publishes **no** cross-currency/cross-frequency credit basis. Such a change MUST be handled as **cancel + new subscription**. Same-currency, same-frequency up/down-grades within one `(currency, region)` are in scope. This launch limitation requires **written sign-off from Subscriptions + Finance + GTM** (Open Questions; operator warning is AC #66).

**Entitlement grant set (catalog -> Subscriptions)** — the Plan MUST publish its entitlement grant set (feature flags, quotas) or its `PlanTier`-resolved reference into the read model; publish MUST fail if a referenced feature, quota, or `PlanTier` policy is undefined. This PRD does **not** define entitlement semantics:

| **Catalog concept** | **Published as (grant set)** | **Consumed by Subscriptions as** |
|---------------------|------------------------------|----------------------------------|
| `Feature` (capability id, PlanTier-driven) | `featureFlag: bool` entry | `Entitlement` (feature access) |
| Quota (limit id + value, PlanTier-driven) | `quotaKey: value` entry | `Entitlement` (usage/resource quota) |

**Plan-change contract (catalog -> Subscriptions)** — Self-service plan changes need two catalog facts this PRD MUST publish; enforcement stays in Subscriptions:

| **Field** | **Requirement** |
|-----------|-----------------|
| `allowedChangeTargets` | The target `planId`s a subscription MAY move to — an explicit list **or** a rule. Absence MUST mean **no self-service change** (fail-safe), **not** any-to-any |
| `comparabilityRank` | An integer rank classifying a change as **upgrade** (higher), **downgrade** (lower), or **switch** (equal). `PlanTier` alone is **not** an ordering unless published as authoritative; otherwise `comparabilityRank` is REQUIRED for any plan that participates in self-service change |

**Tenant policy objects (catalog governance)** — both have fail-safe defaults:

| **Policy object** | **Purpose** | **Safe default** |
|-------------------|-------------|------------------|
| Approval threshold policy | Sets the **material-change** threshold above which the two-person rule applies. Materiality MUST be an absolute amount or a percentage delta (per currency). For a multi-currency change, each affected row's delta is compared in its **own** currency and the rule trips if **any** row exceeds its threshold (AC #64). | If unset, the **two-person rule applies** (fail-safe); the system MUST NOT auto-publish without an explicit threshold |
| Tax-display policy | Decides warn vs. fail when `taxInclusive=true` lacks a region tax rate **and** the symmetric `taxInclusive=false` row in a region with no configured `taxCategory`; governs only the **display basis** | **Fail closed for ALL tenants**; tax **scheme** determination is owned by Tax Engine |

**Rating compatibility contract (normative)**

| **Requirement** | **Statement** |
|-----------------|---------------|
| Stable identifiers | Published plans MUST expose `planId`, `priceId`, `skuId` on all downstream artifacts |
| Snapshot completeness | Publish MUST stamp identifiers sufficient for manifest `pricingSnapshotRef` |
| Model kind | Rows MUST persist `flat` \| `per_unit` \| `graduated` \| `volume` \| `package` — no implicit default at rating time; `per_unit` rows MUST also persist `quantitySource`; `package` rows MUST persist `packageSize`/`packagePrice` |
| Evaluation policy | Usage rows MUST persist `tierAggregationWindow` and `billingGranularity` at publish |
| Billing timing | Recurring rows MUST persist `billingTiming` at publish; usage rows are implicitly `in_arrears`; absence on a recurring row MUST fail publish |
| Meter mapping | Exactly one `meteringUnit` per usage plan revision; ambiguous config MUST fail publish. A **derived (composite) meter** satisfies this as **one output unit** |
| Derived meter (when used) | If a price row uses a derived meter, publish MUST persist constituent units, formula-as-data, and output unit, and MUST fail if any constituent `meteringUnit` is unpublished or the formula self-references; Tariffs evaluates the formula |
| Events | MUST emit `PlanCreated`, `PlanUpdated`, `PlanPublished`, `PlanRetired`; ordered per `(tenantId, aggregateId)` where manifest applies |
| No charge calculation | Catalog MUST NOT compute monetary charges; Tariffs/Rating consume frozen inputs |
| Descriptor completeness | Publish MUST NOT proceed without billing descriptor set |

### 17.7 Advanced Pricing Primitives Detail

**Derived (composite) meter (catalog primitive)** — one price row priced from a **formula across multiple published metering units** (canonical case: VM priced as `vCPU` + `RAM` on one line). This PRD owns the **definition primitive**; Tariffs owns the **evaluation**:

| **Catalog MUST persist** | **Requirement** |
|--------------------------|-----------------|
| Constituent units | >= 2 **published** `meteringUnit` ids (declared by the registry); each MUST be published before the composite can publish |
| Formula expression | The combination rule **as data** (operands + operator/weights), not executable code; catalog stores it, Tariffs evaluates it |
| Output unit | A single declared **derived unit** the price row rates as one line |
| Determinism | The composite definition MUST be **frozen in `pricingSnapshotRef`** so rating is reproducible |
| No computation here | Catalog MUST NOT compute the formula result; it only persists and publishes the definition for Tariffs |

**Prepaid credit grant (catalog primitive)** — this PRD owns the **definition**; balance ledger, drawdown, zero cut-off, and auto-recharge are owned by Billing/Rating (External-dependency GA gate):

| **Catalog MUST persist** | **Requirement** |
|--------------------------|-----------------|
| `grantAmount` | Units granted (> 0), expressed in `creditUnit` |
| `creditUnit` | A currency (ISO 4217) **or** a **published** `meteringUnit`; an unpublished unit MUST fail publish |
| `price` per `(currency, region)` | Purchase price of the grant (>= 0), authored **per `(currency, region)`**. The grant is a **plan-attached primitive**, **not** a `Price` row on the canonical scope key — it carries **no** `chargeKind`. A single unscoped price MUST fail publish for a plan selling in multiple `(currency, region)`. Grant-price changes flow through the **same** material-change policy |
| `expiryPolicy` | `never` or `days(N)` with N > 0; MUST be set explicitly (no implicit "never") |
| `autoRechargeAllowed` | Whether auto-recharge MAY be offered (execution owned by Billing) |
| Determinism | The grant definition MUST be **frozen in `pricingSnapshotRef`**; catalog MUST NOT track balance or compute drawdown |

**Reserved-capacity price (catalog primitive)** — reservation is modeled as **attributes on the single `usage` price row**, **not** a second row, so it does not price the same `(meter, dimensionKey)` twice:

| **Catalog MUST persist (on the usage row)** | **Requirement** |
|---------------------------------------------|-----------------|
| `reservedRate` | Committed unit price (>= 0, row currency) for the reserved/allocated quantity, carried **alongside** the row's on-demand unit price/tiers |
| `reservationFlavor` | `consumption` (matched usage at `reservedRate`, remainder on-demand) **or** `capacity` (allocated quantity billed at `reservedRate` regardless of usage — `capacityCharge`); aligned field-for-field with Tariffs `reservationMatch` |
| Evaluation policy | The row **is** a usage row: `billingGranularity` is REQUIRED, and `tierAggregationWindow` is REQUIRED **only when tiered** — the "forbidden on non-usage rows" rule does **not** apply |
| Quantity source | The reserved/allocated quantity is supplied at runtime (OSS/Contracts entitlement/inventory); catalog MUST NOT meter or compute it |
| Determinism & fixture | Frozen in `pricingSnapshotRef`; reservation is a **Tariffs evaluation variant** of the usage row and MUST carry a joint golden fixture before publish (AC #60; §17.2). Catalog MUST NOT track allocation or compute the charge |

**Boundary**: **negotiated RI-style** reservation rates are carried by Contracts (per-account overlay); **self-service** reservation rates are catalog list pricing (this primitive). Self-service reservation is **in launch Scope**. Tariffs reservation evaluation exists (step 6) and MUST source the **self-service** reserved rate from the catalog snapshot.

**Customer-group pricing (segment overlay)** — modeled as a **`customerGroup` `PriceList` scope**, **not** a new price-row axis:

| **Aspect** | **Requirement** |
|------------|-----------------|
| Mechanism | A **`customerGroup`-scoped `PriceList`** overlay carries an **adjustment** (`markup`/`discount`/`fixed`) per **group x region**, resolved via the payer's group membership on **`payerTenantId`** at `t` (Tariffs evaluates). Entirely different **tier structures** per group are **out of launch scope** (use separate plans — Future) |
| Group taxonomy | A **BSS-owned governed taxonomy** (like `region`/`brand`); group values validated against it at authoring |
| Membership | An **effective-dated, audited BSS record** on the payer's commercial profile (resolved via `payerTenantId`). AMS supplies tenant identity only; the group is a **BSS commercial projection** and MUST NOT change tenant topology |
| Determinism | The **resolved group** MUST be frozen in `pricingSnapshotRef` |
| Membership change | A price-changing operation: **renewal-aligned by default** (pinned in the subscription snapshot until renewal, then re-resolves); an **immediate** re-resolution is allowed as an explicit **material change**. A group discount/move affecting many payers is a **material change** -> two-person rule (AC #28); all membership changes MUST be audited |

Enforcement (overlay evaluation, precedence stacking) is owned by Tariffs; this PRD owns the `customerGroup` `PriceList` **authoring/validation**, the group **taxonomy**, and publishing the **membership** record into the read model.

### 17.8 Future Scope

| **Capability** | **Priority** | **Status** | **Notes** |
|----------------|--------------|------------|-----------|
| Plan comparison (side-by-side) | `p2` | Follow-on | Operator/partner decision support |
| Committed-usage / drawdown flags on plan | `p2` | Cross-PRD | Contracts + Tariffs; catalog may expose reference fields |
| Minimum fee / cap per period on plan | `p2` | Follow-on | Tariffs Future scope |
| Two-dimensional pricing (seats x usage on one plan) | `p2` | Follow-on | Combines per-seat recurring (now in Scope) with a usage meter on the same line; multiple-meter math in Tariffs. **Decision gate** (owner Product): if any launch SKU needs single-line seats x usage it re-scopes out of Future (workaround = two rows) |
| Derived (composite) meter pricing (e.g. VM = vCPU + RAM) | `p2` | Follow-on | **Primitive defined** (§17.7); composes published units from the registry; unblocks Tariffs composite-meter (Tariffs Follow-on) |
| Dimensional pricing (event properties) | `p2` | Deferred | OSS/Rating dimension contract |
| Authorable `aggregationFunction` (`sum` \| `max` \| `last` \| `unique`) / peak-capacity model kind | `p2` | Follow-on | **Decided: not at launch (2026-07-04)** — usage `Q` is always **window-sum**. If peak/period-end count is later needed, add `aggregationFunction` frozen in `pricingSnapshotRef` with Rating/Tariffs semantics + AC #60-style fixture |
| Self-service reserved-capacity rate (reserved/on-demand pair) | — | **Decided -> Scope (2026-07-04)** | Now in **launch Scope** (§17.7); Tariffs sources the reserved rate from the catalog snapshot; negotiated RI-style rates stay in Contracts |
| Percentage-of-base pricing model kind | `p2` | Follow-on | Tariffs Future scope |
| Structural free-tier / freemium flag (beyond per-row $0) | `p2` | Follow-on | Per-row `$0` is **now publishable**; a first-class freemium plan flag remains Future. **Decision gate** (owner Product + Analytics): if reporting cannot distinguish a freemium plan from a `$0` promo band, the flag re-scopes into launch |
| First-class `includedAllowance` (+ `rolloverPolicy`) on usage rows | `p2` | Follow-on | **Decided: keep the `$0`-first-band (2026-07-04)**; display/reporting limitation recorded in Glossary. Rollover **evaluation** stays downstream |
| Self-service term / auto-renew metadata on Plan | `p2` | Cross-PRD | **Decided: not at launch (2026-07-04)** — explicit boundary (Out of Scope). If later needed, add optional `termLength`/`autoRenew` frozen in snapshot |
| Customer-group **different-tier-structure** per group | `p2` | Cross-PRD | **Decided (2026-07-04, F-88): the adjustment-based segment overlay is now in launch Scope** (§17.7). What stays Future: a group needing an **entirely different tier structure** — that requires separate plans + group-scoped plan eligibility |
| Typed credit/discount price rows (negative amounts) | `p2` | Follow-on | Negative amounts allowed only on explicitly typed credit rows; today rejected |
| Interim catalog `discountRef` reference field | — | **Decided -> Scope (2026-07-04)** | Committed as the **day-1 discount hook** (referential-integrity only — no authoring/evaluation here). A full Promotions PRD remains the durable owner |
| Minimal in-catalog list-discount price row | `p2` | Conditional | Last-resort fallback if both Promotions PRD and `discountRef` are insufficient at launch: a typed list-discount row (referential-integrity only, no stacking/evaluation here). Decision tied to the day-1 discount program gate |
| Explicit `currencyFallbackPolicy` (FX fallback) | `p3` | Deferred | Default is fail-closed (no implicit FX); fallback requires a configured policy |
| Catalog `refundable` / `creditPolicy` per price row | `p3` | Cross-PRD | Refund execution owned by Billing/Payments; catalog flag only if Finance requires it. **Decision gate** (owner Finance + Billing): if cancellation credits are required at launch, the per-row `cancellationCreditBasis` (conditional AC #86, fail-safe = no credit) re-scopes into launch |
| Marketplace listing eligibility rules per plan | `p3` | Follow-on | Tied to manifest §4.8 |

---

*Child artifacts: ADR(s) for canonical-scope-key and snapshot-versioning strategy; DESIGN for the catalog read model, publish/validation pipeline, and Tariffs / Subscriptions / Registry / Billing integration contracts.*




