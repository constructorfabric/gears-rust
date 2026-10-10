---
refs:
  - bss/manifest/vz-arch-manifest-bss-only.md
  - bss/prd/PRD-billing-ledger-balances-202604041200/PRD-billing-ledger-balances-202604041200.md
  - bss/prd/PRD-contracts-agreements-202601120119/PRD-contracts-agreements-202601120119.md
  - bss/prd/PRD-metering-pricing-module-202601120119/PRD-metering-pricing-module-202601120119.md
  - bss/prd/PRD-product-catalog-marketplace-202601120119/PRD-product-catalog-marketplace-202601120119.md
  - bss/prd/PRD-rating-engine-202604031200/PRD-rating-engine-202604031200.md
  - bss/prd/PRD-subscriptions-lifecycle-202604021200/PRD-subscriptions-lifecycle-202604021200.md
---

Created:  2026-08-24 by Virtuozzo International GmbH
Updated:  2026-10-08 by Virtuozzo International GmbH

# PRD — Rating — Usage Rating & Commercial Pricing Logic

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
  - [5.3 Release Gates](#53-release-gates)
- [6. Functional Requirements](#6-functional-requirements)
  - [6.1 Deterministic Evaluation](#61-deterministic-evaluation)
  - [6.2 Pricing Models](#62-pricing-models)
  - [6.3 Rule Evaluation Order](#63-rule-evaluation-order)
  - [6.4 Override Hierarchy (overlays superseded)](#64-override-hierarchy-overlays-superseded)
  - [6.5 Rating Window, Eligibility, Phases, Granularity](#65-rating-window-eligibility-phases-granularity)
  - [6.6 Commitments and Reservations](#66-commitments-and-reservations)
  - [6.7 Dimensional (Cloud) Pricing](#67-dimensional-cloud-pricing)
  - [6.8 Coupons (Promotions Overlay)](#68-coupons-promotions-overlay)
  - [6.9 Multi-Currency and FX](#69-multi-currency-and-fx)
  - [6.10 Retroactivity and Corrections](#610-retroactivity-and-corrections)
  - [6.11 Period-Level and Plan-Change Obligations](#611-period-level-and-plan-change-obligations)
  - [6.12 Governance and ASC 606 Traceability](#612-governance-and-asc-606-traceability)
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
  - [Price resolution and determinism](#price-resolution-and-determinism)
  - [Pricing models](#pricing-models)
  - [Time, versioning, currency](#time-versioning-currency)
  - [Retroactivity and corrections](#retroactivity-and-corrections)
  - [ASC 606 traceability](#asc-606-traceability)
  - [Rating window, renewal and minimum fee](#rating-window-renewal-and-minimum-fee)
  - [Promotions and coupons](#promotions-and-coupons)
  - [Plan change and proration](#plan-change-and-proration)
  - [Cloud resource pricing](#cloud-resource-pricing)
  - [Non-Functional Requirements (Show-Stoppers)](#non-functional-requirements-show-stoppers)
- [13. Dependencies](#13-dependencies)
- [14. Assumptions](#14-assumptions)
- [15. Open Questions](#15-open-questions)
- [16. Risks](#16-risks)
- [17. Reference Materials](#17-reference-materials)
  - [17.1 Evaluation Order (normative appendix)](#171-evaluation-order-normative-appendix)
  - [17.2 Boundary Contracts (coupons, floor/cap, plan-change proration)](#172-boundary-contracts-coupons-floorcap-plan-change-proration)
  - [17.3 Cloud Catalog Readiness and Phasing](#173-cloud-catalog-readiness-and-phasing)
  - [17.4 Future Scope](#174-future-scope)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

**Rating** is the BSS gear that turns metered usage and subscription state into **deterministic, auditable charges**. Per [ADR-0002](./ADR/0002-cpt-cf-bss-rating-adr-rating-gear-consolidation.md) it consists of two parts in one deployable: the **evaluation core** (`rating-core`) — a pure function that resolves **effective commercial prices and charge formulas** for subscriptions and usage meters in a **multi-tenant hierarchy** (platform owner → channel partner / reseller → end customer), under **usage-based and hybrid commercial models**, with **financial-grade auditability** and **byte-for-byte reproducible** outputs — and the **operational pipeline** (usage ingestion, windowed `Q` aggregation, dedup, evaluation-unit synthesis, rated-output persistence, handoffs — design contracts 12–16). **Evaluation** (§6.3 / §17.1) prices the **bindings Pricing resolves** for a subscription and emits a **resolved price outcome** plus a `pricingSnapshotRef` (Rating's snapshot).

This gear owns the **evaluation semantics and the rating pipeline — not the price definitions or their selection**: price books, entries per SKU, dated prices per dimension-value chain, plans and plan revisions, and their approval are authored and owned by the **Pricing** gear (the PriceBook model, pricing D-384…D-397); SKUs and derived usage types are owned by **Products**. Pricing also selects the price: `resolve` binds the price in force for a signup and walks a pinned chain for a renewal (pricing D-420). Rating consumes those bindings and keeps each one it rates with (*adopt, don't fork* — [adopted pricing semantics](./DESIGN.md#adopted-pricing-semantics); DECISIONS T-D-73, [ADR-0003](./ADR/0003-cpt-cf-bss-rating-adr-pricebook-bindings.md)). It does **not** compute tax, recognize revenue, manage coupon lifecycle, or enforce spend — those remain in their owning domains (§5.2).

### 1.2 Background / Problem Statement

BSS must monetize a real IaaS catalog (S3, VM, Disks) across a partner/reseller hierarchy with usage-based, hybrid, and committed-usage models. Without explicit, normative rating semantics, rating batches, replay, and late-arrival handling diverge — producing non-reproducible charges, disputes, and non-auditable financials.

This PRD fixes the **formula semantics** (flat, per_unit, graduated, volume, package, hybrid; committed usage when a Contracts source exists), the **deterministic evaluation order**, the **multi-currency** separation (price vs billing currency vs FX policy), and **CFO-grade controls** (binding-of-record audit, UTC dating, ASC 606-compatible tagging; segregation of duties on publish is Pricing's approval engine) so Design implements — not invents — these rules.

Industry alignment: usage-based pricing platforms (Metronome, Lago, OpenMeter) are the coverage/sequencing benchmark for cloud model breadth (dimensional, composite, capacity/reservation); partner and customer price differences are expressed in Pricing as separate price books or SKUs (Pricing dropped price overlays); retroactivity never mutates posted invoices: Rating publishes a complete new result revision with lineage and Billing turns the difference against what it posted into an adjustment (manifest §4.2; T-D-50).

### 1.3 Goals (Business Outcomes)

- **Determinism**: given frozen inputs `(window-aggregated inputs, binding of record, engine generation)` (plus an `fxTableVersion` once FX exists), the monetary outcome is identical across replay, recompute, and cross-region batch workers; all divergences without input change are defects.
- **Model coverage**: the pricing models `flat`, `per_unit`, `graduated`, `volume` and `package` (pricing D-386), hybrid (recurring + usage lines) and derived (composite) usage meters are supported with explicit semantics; committed usage (drawdown + overage + true-up) is release-gated on a Contracts source (§5.3).
- **Multi-currency correctness**: price currency, invoice/settlement currency, and FX policy (rate-lock per window or invoice-period FX) are separated; no implicit provider-default FX.
- **Auditability**: every result references the binding of record it was rated with (plan revision, pins, price ids and money digests), UTC dating, and ASC 606-compatible allocation inputs (PO tags, SSP pointers) carried as references — not a recognition engine. The two-person rule on publish is Pricing's (the shared `bss-approval` engine).
- **Scale**: horizontal evaluation per tenant/partition with bounded p95 latency and no cross-partition locks on the hot path (working-assumption targets in §7.1 until NFR workshop).

### 1.4 Glossary

| **Term** | **Definition** |
|----------|----------------|
| **Tariff** | Reserved for **Pricing-owned rate definitions** (price books, entries, prices). Since ADR-0002 it no longer names this gear. |
| **Price book / entry / price** | Pricing's PriceBook model (pricing D-384…D-397): a **price book** has one currency; an **entry** is a SKU × charge kind × period × model (× usage-rating-policy digest) line of a book with at most one dimension key; a **price** is one dated amount of one dimension-value chain (`effective_from`, `effective_to`, `temporary_until`, `eligibility: all \| new`, state, `min_fee`), approved through Pricing's approval units. |
| **Plan revision** | The published (or scheduled, or superseded) revision of a Pricing plan; it binds one book and lists plan items (a SKU and its entry). Rating rates against the `revision_id` a fact names. |
| **Pin** | A subscription's bound price, sent to Pricing's `resolve` as `PricePin{item_id, dimension_value?, price_id}`. **Subscriptions owns the pins per period** (SUB-D-29; the store and the fact field that carries them are PROPOSED — UPSTREAM_REQS §2.6); the first period's pins are the accepted order's bindings (Orders D-162). Pricing stores none. |
| **Binding** | One cell of Pricing's `resolve` answer (`AcceptedBinding`): the bound price (`ImmutablePrice` — money in exact major-unit decimals, `minimum_fee`, `effective_from`, `ends_on`, money digest), the SKU version, the usage rating policy and meter for usage, and the invoice inputs (`InvoiceInputs`). An uncovered cell has no binding and is never free. |
| **Binding of record** | The `(revision_id, date, pins)` a child window was finalized with and the bindings `resolve` returned, stored verbatim by Rating (DECISIONS T-D-73). Corrections reuse it; replay reads it; only an administrative re-rate replaces it. Supersedes the "pin-of-record" over a catalog version. |
| **`ends_on`** | A binding's own end — a temporary price's end or an explicitly closed price's end (pricing D-425). The only point at which Rating slices a period by price; `effective_to` never is one. |
| **Usage rating policy** | The immutable `UsageRatingPolicy` of a usage entry (pricing D-502, D-514): `rating_window ∈ {BillingCycle, CalendarHour(UTC)}`, `aggregation_scope ∈ {subscription_line, resource}`, `reset = rating_window_start`, `partial_window = actual_quantity_full_thresholds`, `fold = SUM`. It replaces `tierAggregationWindow` (DECISIONS T-D-75). |
| **Derived usage type** | A meter Products declares as versioned data over raw collector usage types (`products.derived/<code>@<n>`; products P-D-229, P-D-259). Rating folds each input per UTC hour, evaluates the formula per hour and sums the hours into the window quantity (DECISIONS T-D-76). |
| **Minimum fee** | A price's optional `minimum_fee` (pricing D-388); Rating applies it per price, per subscription, per billing period, before promotions, prorated by coverage (DECISIONS T-D-38, T-D-78). |
| **Resolved price outcome** | The output of one **evaluation**: the bindings priced, model kind, tier thresholds, exact amounts and snapshot identifiers — not a separate Pricing entity. |
| **Evaluation context** | Inputs to resolve one price outcome: tenant axes (`resourceTenantId`, `payerTenantId`, `sellerTenantId`), the commercial fact (subscription line, billing period, plan revision, pins, quantity), the binding of record, the usage rating policy and derived declaration for usage, windowed quantities per slice, timestamp `t` (UTC), and the engine generation. **`periodState`** (`open` \| `closed_posted`) is Billing's and is not an evaluation input (T-D-50). Optional **`reservationMatch`** and contract or coupon inputs have no source today (release-gated, §5.3). |
| **periodState** | Open/closed state of the billing period covering `t`. **Billing owns it.** Design (DECISIONS R-04 revised, T-D-50 — proposed, following the Seam Atlas v2 D09): Rating publishes every correction as a complete, absolute new result revision and never reads `periodState`; Billing applies a revision to an open draft or posts a credit/debit note of the rounded corrected line total minus the cumulative posted amount, never a rounded difference (money ADR rule 8; DECISIONS T-D-80). The posted-period and delta requirements below (§6.10) are therefore satisfied jointly: Rating never mutates a delivered result, Billing never mutates a posted invoice. |
| **reservationMatch** | Optional input describing reserved/provisioned capacity at `t`: reserved rate, reserved/allocated quantity (`reservedQuantity`), and an optional usage-coverage flag. Two charge flavors: **(a) consumption-flavor** (matched usage at reserved rate, remainder on-demand); **(b) capacity-flavor** (allocated quantity charged at reserved rate regardless of usage). **No source exists**: Pricing has no reserved rates and leaves reserved capacity to a later decision; Contracts is a first-draft PRD (release-gated, §5.3). |
| **capacityCharge** | The capacity-flavor charge: a recurring-style charge on `reservedQuantity` (e.g. provisioned-disk GB, provisioned IOPS) at the reserved rate, emitted per period independent of usage; dormant with `reservationMatch`. |
| **Rating window** | The window over which tier counter `Q` accumulates and resets, from the binding's usage rating policy: `BillingCycle` — the fact's billing period; `CalendarHour` — a UTC clock hour (pricing D-514). Tiers restart at each window start (`reset = rating_window_start`); a clipped first or last window rates its actual quantity against whole thresholds (`partial_window`). Thresholds are half-open `[lower, upper)` — a quantity at a boundary falls in the UPPER band. Slices inside one window (a binding's `ends_on`, a plan change inside the window) do not reset the counter: counter continuation follows T-D-36. *Superseded*: `tierAggregationWindow` and its values `calendar_month`, `invoice_period`, `subscription_lifetime`, `per_event`, `per_hour`. |
| **Billing granularity** | Minimum billable unit for a usage quantity. Pricing on `main` publishes no granularity field; Rating rounds nothing up unless a derived usage type's own formula does (`output_scale`, `output_round`; products P-D-229). A per-resource `minimumCharge` MAY bound ephemeral-resource over-charge (§15). |
| **dimensionKey** | The one optional dimension key of a pricing entry (a tenant registry seeded with `region`; pricing D-385); each value has its own price chain, and a null value is the default chain. One charge line per `(skuId, dimensionKey)` value. Value emission on usage is an OSS/Rating contract (R-16). |
| **Proration** | Rating's (pricing D-388, D-415; Subscriptions ADR-0002 WHEN/MATH): the covered fraction of a billing period is covered UTC seconds / the period's UTC seconds, exact (DECISIONS T-D-77; confirmation open, R-30). *Superseded*: the `prorationBasis` enum. |
| **Price eligibility** | *Superseded (2026-10-08).* A price's `eligibility: all \| new` is an input of Pricing's renewal walk (pricing D-420), not a Rating selection rule; the classes `all_subscriptions` / `new_subscriptions_only` / `existing_grandfathered` and cohorts no longer exist (pricing D-397). |
| **Plan phase** | *Superseded (2026-10-08).* Pricing has no phases or trials (pricing PRD; D-504 refuses phases in a new sale). One-time charges are deduplicated by Subscriptions per phase-entry occurrence (SUB-D-28). |
| **CatalogVersion** | *Superseded (2026-10-08).* No catalog version exists in Pricing or Products; Rating identifies prices by plan revision, pins and price ids, and SKUs by SKU version (`sku_version`). |
| **pricingSnapshotRef** | Rating's snapshot of everything it rated with: the plan and plan revision, the resolve date, the bindings (price ids, money digests, SKU versions, usage rating policies, invoice inputs) and their digest, the derived declaration digest for usage, the fact version, and the engine generation. Rating composes it and is its only writer (Design: content-addressed, tenant-owned `bss_rating__snapshot`; T-D-44). Billing passes its id to the ledger as `pricing_snapshot_ref`. **Not** the upstream *accepted price binding* — the bindings the subscription accepted, which Subscriptions' documents still name `pricingSnapshotRef` and Orders calls a retired term — which Rating checks its resolved bindings against (`binding_mismatch`) and never writes. The shared name is a CONTRACT CONFLICT owed jointly by Subscriptions, Billing and Rating (DECISIONS R-29); in this PRD `pricingSnapshotRef` means Rating's snapshot unless the text says "accepted price binding". |
| **PlanTier** | *Superseded (2026-10-08).* Out of scope in Products (products PRD); not an evaluation input. |
| **OrgTier overlay** | *Superseded (2026-10-08).* Pricing has no price overlays; a partner's or customer group's prices are a separate price book (a mapping of customer groups to books is a later Pricing decision). |
| **Committed usage** | A committed quantity or spend pool (**commitment pool**, Contracts SoR) drawn down by metered usage; overage and true-up follow committed/overage rates. Two pool flavors (frozen `poolType`): `prepaid_drawdown` — the pool is billed upfront at sale (outside rating-core) and in-commit consumption is due-zero with notional lineage; `committed_rate` — in-commit consumption bills in arrears at the committed rate, with a period-end shortfall true-up. No source exists (release-gated, §5.3). |
| **True-up obligation** | Period-end commercial adjustment surfaced as a structured `TrueUpObligation` on the evaluation result (amount, period, contract ref) for Billing — not a silent in-engine charge. Dormant with committed usage. |
| **Mid-cycle change** | A binding that ends (`ends_on`) or a plan change that takes effect inside the subscriber's current billing period. |
| **Retroactive pricing** | Any rule assigning a rate to usage based on a policy decision time earlier than operational processing time (late arrival, administrative re-rate). Pricing cannot change an approved price retroactively (approved prices are immutable and cannot start in the past), so a retroactive price for a subscription arrives as new pins on a new fact version. |
| **PriceWindow** | *Superseded (2026-10-08).* The window is fields on each price (`effective_from`, `effective_to`, `temporary_until`, explicit close); Pricing's `resolve` chooses the price in force (pricing D-390, D-420). |
| **PriceOverlay** | *Superseded (2026-10-08).* Removed from Pricing (PriceBook spec item 2); a plan-specific exception uses another price book or SKU. |
| **Coupon** | Promotional discount instrument (id, type, validity, applicability, redemption limits, campaigns). Promotions are owned by Pricing and deferred (pricing D-409); this PRD owns when and how an eligible coupon would adjust a resolved charge line. |
| **Coupon stacking policy** | `exclusive_best` (default — single winning coupon) or `ordered_stack` (explicit campaign-linked sequence only). |
| **RatingRule** | Defined in manifest §4.2 — maps resolved price outcome to Usage → RatedCharge in Rating. Not redefined here. |
| **SSP (Standalone Selling Price)** | Price at which an entity would sell a promised good/service separately; an input to ASC 606 allocation. Carried as references on charge lines; recognition schedules are out of scope. |

## 2. Architecture Alignment

| **Field** | **Value** |
|-----------|----------|
| **Applicable Manifest(s)** | BSS |
| **Relevant Chapters** | §4.1 Product and Service Catalog; §4.2 Rating and Charging; §4.4 Billing and Invoicing (snapshot/immutability contract); §2.1.3 Multi-tenant semantics; §8 Data and Domain Model (identity invariants) |

> **Normative alignment**: extends manifest requirements for **commercial price resolution** and **deterministic rating inputs**. MUST NOT contradict: (a) Pricing and Products as SoR for SKUs, price books, entries, prices, plans and plan revisions, and Pricing as the selector of the bound price; (b) Rating as deterministic Usage→RatedCharge→BillableItem pipeline; (c) posted financial immutability with corrections via adjustments/credit/debit notes; (d) OSS/BSS boundary (BSS MUST NOT mutate OSS topology or Policy Engine state).

> **Manifest extension (price coverage)**: manifest §4.1 guarantees non-overlapping windows for a key. On `main` the guarantee is Pricing's: one approved start per chain and half-open windows (pricing D-390), and `resolve` answers each cell with a binding or `uncovered` (D-420). This PRD additionally requires **no gaps** for billable usage at `t`: an uncovered cell MUST fail explicitly (`price_uncovered`, AC 6) and is never priced at zero.

> **Deployment (normative for Design)**: the evaluation core (**`rating-core`**) is a **pure, I/O-free crate inside the single `rating` gear/deployable** — consolidation per [ADR-0002](./ADR/0002-cpt-cf-bss-rating-adr-rating-gear-consolidation.md), which supersedes the earlier placement note that treated the evaluation core as a logical module within the BSS Rating domain; the constraint it protected (one deployable, no separate evaluation service) is preserved and strengthened to a compiler-checked crate boundary.

### 2.1 Terminology and Naming

| **Name** | **Usage** |
|----------|-----------|
| **Rating** | Canonical name of this gear and its domain (manifest §4.2): the evaluation core plus the operational rating pipeline — one gear, one deployable (ADR-0002). |
| **rating-core** | The pure evaluation core (crate): deterministic pricing of resolved bindings (§6.3 / §17.1) over frozen inputs, no I/O. Successor of "tariff-core"/"PLAL" (the pre-ADR-0002 names); use at implementation/abstraction boundaries. |
| **Rating pipeline** | The operational half: usage ingestion & normalization, attribution projection, windowed `Q`, usage and result dedup, child-window scheduling beneath commercial facts, finalization, result persistence and roll-up, Billing delivery, `CommitmentBalanceEffect` publication (dormant) (design contracts 11–16). |
| **Evaluation** (historically "evaluation") | The deterministic process that prices the bindings Pricing resolved for a given context (§6.3). Produces a resolved price outcome + `pricingSnapshotRef`. |
| **Tariff** | Reserved for the **Pricing gear's rate definitions** (price books, entries, prices). Since ADR-0002 it no longer names this gear or its process. |
| **Tariffs / PLAL / tariff-core** | Deprecated names for this gear / its core — do not use in new text (ADR-0002); historical occurrences (pricing documents, ADRs) read per ADR-0002. |

### 2.2 Predecessor PRDs and Scope Migration

This PRD specializes or supersedes the following scope from predecessor documents:

- **PRD-metering-pricing-module-202601120119** — "Pricing Hierarchy Orchestration (Contract > PriceOverlay > Catalog)" and "Tiered Pricing Calculator" (P0 Rating scope) moved here as the §6.3 evaluation order (§17.1) and formula definitions; on `main` the hierarchy itself is gone (Pricing has no overlays and selects the price). The metering/collection half remains authoritative there.
- **PRD-product-catalog-marketplace-202601120119** — "Plan & Price Modeling", "Effective Dating & Price Windows", and "Price Overlays & Adjustments" defined the **data primitives** this PRD evaluates; on `main` they are Pricing's PriceBook model (price books, entries, dated prices, plans and revisions — overlays were dropped) and Products' SKU registry. Evaluation semantics are authoritative here; price selection is Pricing's `resolve`.
- **PRD-rating-engine-202604031200** (VHP-810) — **absorbed by this PRD (ADR-0002)**: its HIGH scope items ("Tiered pricing evaluation", "Deterministic outputs + pricingSnapshotRef") are specified here (§6), and the pipeline scope it stubbed (Usage → RatedCharge, dedup, windowed `Q`, evaluation-unit synthesis, persistence, Billing handoff) is authored as design contracts 12–16 of this gear. The upstream copy is legacy provenance (not maintained).

## 3. Actors

### 3.1 Human Actors

#### Product Manager

**ID**: `cpt-cf-bss-rating-actor-product-manager`

**Role**: Defines price books, entries, prices, plans and derived usage types (in Pricing and Products) so commercial behavior is explicit.
**Needs**: Pricing's price-book and plan editors, model configuration (flat / per_unit / graduated / volume / package), UTC dated prices, approval submit (Pricing's approval units).

#### Partner Admin

**ID**: `cpt-cf-bss-rating-actor-partner-admin`

**Role**: Maintains a partner's or customer group's prices so channel economics are controlled.
**Needs**: a separate price book or SKU per partner in Pricing (Pricing dropped price overlays; mapping customer groups to books is a later Pricing decision). *Superseded (2026-10-08)*: OrgTier scope selection and adjustment stacks.

#### Finance Analyst

**ID**: `cpt-cf-bss-rating-actor-finance-analyst`

**Role**: Previews invoice impacts of a future window so forecasts and ASC inputs are explainable.
**Needs**: Sample-usage profiles, candidate-window selection, evaluation-trace export.

#### Platform Operator

**ID**: `cpt-cf-bss-rating-actor-platform-operator`

**Role**: Owns deterministic, hierarchical price resolution so usage-based revenue is reproducible, auditable, and compatible with Rating and Finance controls.
**Needs**: Audit of rule versions, segregation of duties on publish, deterministic replay.

### 3.2 System Actors

#### Rating Pipeline (intra-gear)

**ID**: `cpt-cf-bss-rating-actor-rating`

**Role**: The operational half of this gear (design contracts 11–16) — consumes the core's resolved price outcome in-process; produces exact window results and complete parent-fact results delivered to Billing (`BillableItemDeliveryV1`, the Design realization of manifest `RatedCharge` / `BillableItem`); owns usage intake, usage and result dedup, windowed `Q` aggregation per child window and aggregation key, the child-window scheduler beneath Subscriptions' commercial facts, finalization, result persistence, and `CommitmentBalanceEffect` publication (dormant). Listed with the system actors because the evaluation-core FRs (§6) reference it as their operational counterpart; since ADR-0002 it is intra-gear, not an external system.

#### Billing & Invoicing

**ID**: `cpt-cf-bss-rating-actor-billing`

**Role**: Consumes Rating's complete exact result revisions + snapshots; owns period state (open / frozen / posted) and decides draft replacement vs credit/debit note; rounds each invoice-line aggregate once; posts immutable invoices; executes period-level floor/cap. No Billing gear exists yet (DECISIONS R-04, R-05).

#### Pricing (Product Catalog)

**ID**: `cpt-cf-bss-rating-actor-catalog`

**Role**: the **Pricing** gear (PriceBook model, pricing D-384…D-397) — SoR for price books, entries, dated prices, plans and plan revisions, and the selector of the bound price. Rating reads it through `PricingReadV1::{resolve, price, current_revision}` (`GET /bss-pricing/v1/resolve`, `GET /bss-pricing/v1/prices/{id}`; pricing D-419…D-425) as the system subject `bss-rating.system` with pricing `plan:read` and `price:read` (D-424). Pricing publishes `prices_published`, `plan_revision_published` and approval events; Rating consumes none of them (DECISIONS T-D-73).

#### Contracts & Agreements

**ID**: `cpt-cf-bss-rating-actor-contracts`

**Role**: Supplies account-specific price terms, commitments and true-up clauses. First-draft PRD only; no source exists (release-gated, §5.3).

#### Subscriptions

**ID**: `cpt-cf-bss-rating-actor-subscriptions`

**Role**: Owns effective-dated Plan/Add-on links, subscription state, billing periods and `BillingTerms`, the committed quantity, the subscription's **price pins per period** (SUB-D-29), and the plan-change WHEN/asymmetry policy (`changeEffectiveAt`, `changeMode`).

#### Promotions / Discounts

**ID**: `cpt-cf-bss-rating-actor-promotions`

**Role**: Owns the Coupon entity and campaign stacking; supplies frozen coupon snapshots. Rating consumes; never mutates campaigns. Promotions are owned by Pricing and deferred (pricing D-409); no source exists.

#### Finance (FX)

**ID**: `cpt-cf-bss-rating-actor-finance-fx`

**Role**: Owns FX rate tables and lock policies; rating-core consumes them as frozen inputs and records `fxTableVersion` / locked-rate id.

#### OSS / AMS (Tenant Identity)

**ID**: `cpt-cf-bss-rating-actor-oss-ams`

**Role**: Supplies `tenantId` and delegation proofs.

#### OSS Metering (Usage Dimension Population)

**ID**: `cpt-cf-bss-rating-actor-oss-metering`

**Role**: Emits `dimensionKey` values on each UsageRecord (e.g. S3 storage-class / region / operation; VM instance type) and normalized usage quantity. **Critical-path upstream dependency** for dimensional pricing.

## 4. Operational Concept & Environment

### 4.1 Module-Specific Environment Constraints

- **Multi-tenant isolation**: price books, bindings and contract overrides are tenant-scoped; cross-tenant administration requires delegation proofs; a contract/account override MUST NOT leak across payer/seller tenant scope.
- **Time**: all dating and window boundaries are in **UTC**; a `CalendarHour` rating window is a UTC clock hour; a `BillingCycle` window is the billing period the commercial fact carries (Subscriptions cuts periods from its `BillingTerms`: cycle `Month`/`Year`, anchor `Calendar`/`SubscriptionStart`, UTC).
- **Determinism boundary**: rating-core is a pure, I/O-free crate within the `rating` gear; it consumes frozen inputs (the binding of record, the derived usage declaration, windowed `Q`; FX tables and coupon snapshots once sources exist) and MUST NOT re-query Pricing for a correction or replay.
- **Decimal precision**: rating-core emits amounts at precision sufficient for Billing; invoice rounding (per-line vs per-invoice) is applied by Billing, not rating-core. Design: amounts are exact fractions in major currency units carrying `currency` and `currency_scale`, with no intermediate rounding; Billing rounds each invoice-line total once, HALF_EVEN, at the stored `currency_scale` and computes corrections against cumulative postings (DECISIONS T-D-51, T-D-80, R-14; the proposed cross-BSS money ADR, PR #5270).

**Event alignment (manifest §4.1-4.2)**:

- MUST observe every price change that reaches a subscription — a new price on a chain, an explicit close, a cancellation — through the pins Subscriptions sends and the bindings `resolve` returns, never by reading mutable price state at bill-post time. A cancelled price is never in force (pricing D-520). (Design reads bindings by pull; Pricing's `prices_published` / `plan_revision_published` events are not consumed — DESIGN §3.5.)
- MUST NOT require Rating to re-query Pricing at bill-post time for posted periods; the binding of record remains authoritative.

> **Gating dependency (critical path for IaaS billing)**: the **usage dimension-population contract** (OSS metering → rating pipeline ingestion → rating-core) is the bottleneck for billing real cloud resources. The BSS side is owned here (Pricing declares one dimension key per entry; Rating records the value bound in the binding of record and passes it through). The external part is **OSS metering emission** of dimension values: until OSS emits them, `dimensionKey` stays the empty tuple and the only workaround is minting a separate meter per dimension combination — exploding catalog cardinality. See §17.3 and §15.

## 5. Scope

### 5.1 In Scope

| **Feature** | **Priority** | **Notes** |
|-------------|--------------|-----------|
| Deterministic evaluation API (conceptual contract) for Rating | `p1` | Resolved outcome over the binding of record + `pricingSnapshotRef` + metadata; replay-safe (§6.1). |
| Pricing models: flat, per_unit, graduated, volume, package, hybrid | `p1` | Pricing's `PriceModel` (pricing D-386): usage = per_unit / graduated / volume / package; recurring and one_time = flat / per_unit. Money in exact major-unit decimals (DECISIONS T-D-74). Committed usage is release-gated (§5.3). |
| Binding of prices by Pricing; binding of record kept by Rating | `p1` | `resolve` with the subscription's pins at the period start and at each `ends_on` (pricing D-397, D-425; DECISIONS T-D-73, ADR-0003). Supersedes Rating-side selection on a scope key. |
| Multi-currency: price currency, conversion policy, rate-lock hooks | `p1` | One currency per price book; FX is release-gated (§5.3); no tax calculation. |
| Override hierarchy: global → region/brand/orgTier/partner → customerGroup → customer/contract with explicit precedence | `p1` | **Superseded (2026-10-08)**: Pricing has no price overlays; partner and customer-group prices are separate books or SKUs. Contract overrides remain release-gated (§5.3). |
| PriceOverlay scope → tenant-axis mapping (seller/payer/brand/region) | `p1` | **Superseded (2026-10-08)**: no overlays exist. |
| Rating window (`Q` reset policy) for tiered/volume models | `p1` | From the binding's usage rating policy: `BillingCycle` or `CalendarHour` (pricing D-514; DECISIONS T-D-75); AC 12. |
| Plan phases (trial / intro / evergreen) — price resolution per active phase | `p1` | **Superseded (2026-10-08)**: Pricing has no phases (pricing PRD; D-504). |
| Price eligibility / grandfathering (new vs existing subscriptions) | `p1` | **Superseded (2026-10-08)**: Pricing's renewal walk keeps a pinned subscription on its price (`all` taken, stops before `new`; pricing D-420). |
| Billing granularity (minimum billable unit per usage price) | `p1` | Pricing publishes no granularity; a derived usage type's own rounding applies (products P-D-229); AC 15. |
| Dimensional pricing — `(skuId, dimensionKey)` lines | `p1` | One dimension key per entry, a price chain per value (pricing D-385); step 2 + AC 3 + AC 18. Depends on the usage dimension-population contract (R-16). |
| CAPACITY / reservation pricing (provisioned Disks/IOPS, RI-style) | `p1` | Release-gated (§5.3): no reserved-rate or reservation-match source exists. |
| Usage dimension-population contract (BSS side owned here; OSS emission external) | `p1` | Gating dependency. Rating records and passes `dimensionKey` through; OSS emits values (external). |
| Level-based (gauge) aggregation — `Peak` / `TimeWeighted` input folds of a derived usage type | `p1` | **Suspended** (DECISIONS R-06; usage-collector PRD and Seam Atlas R1 require emitter pre-integration into `SUM` meters). The fold is the derived usage type's per input (products P-D-229; T-D-17 amended by T-D-39); only `Sum` inputs are rated at launch; §6.2 `fr-level-aggregation`, AC 5d. |
| Composite (derived) meter evaluation | `p1` | Products declares the derived usage type (≥ 1 input; P-D-229, P-D-259); Rating evaluates it per UTC hour (T-D-39, T-D-76); needs a Products typed read (R-31); §6.7. |
| Minimum fee per price | `p1` | Rating applies `minimum_fee` per price, per subscription, per billing period, prorated (pricing D-388; DECISIONS T-D-38, T-D-78); §6.11. |
| Bundle `sum_of_parts` component summing + effective rev-share pass-through | `p1` | **Superseded (2026-10-08)**: Pricing refuses bundle SKUs as priceable (`BUNDLE_SKU_NOT_PRICEABLE`); the sold-as bundle is deferred (pricing D-411). |
| Coupon application in evaluation (order, stacking, tier/FX interaction) | `p2` | Promotions are owned by Pricing and deferred (D-409); semantics in §17.2; AC 16; release-gated (§5.3). |
| Mid-cycle price changes: slice at `ends_on`, proration in UTC | `p2` | No posted invoice mutation; a binding's `ends_on` is the only price slice point (pricing D-425). |
| Retroactive pricing modes: administrative re-rate → new complete result revisions; Billing derives the adjustment | `p2` | Preserves invoice immutability. A re-rate re-binds with the fact's current pins or moves to a new engine generation (T-D-73); Billing derives the adjustment (T-D-50). |
| ASC 606 alignment hooks: PO tags, SSP snapshot pointers, allocatable amount fields | `p2` | Recognition schedules remain Billing/Finance; Rating supplies traceable inputs. |
| Operator UX for tariff maintenance, simulation, approval thresholds | `p2` | Price maintenance and its approval are Pricing's UI and engine (`bss-approval`); Rating's operator plane is in DESIGN §3.3. |

### 5.2 Out of Scope

- **API schemas, storage DDL, error code taxonomies** — Design document(s).
- **OSS metering** emission shapes and `UsageRecord` content beyond fields Rating consumes — OSS domain; Pricing consumes aggregated dimensions per the Rating contract.
- **Tax determination** and **statutory invoicing** — Tax Engine / Billing (rating-core MUST NOT compute tax). Handoff: the emitted amount MUST carry **discount lineage** (pre-/post-contract-override, pre-/post-coupon amounts and applied ids) so Billing/Tax can choose gross-vs-net treatment; Rating supplies lineage, not ordering.
- **Full revenue recognition subledger** and **ASC 606 automated journal entries** — Finance/Billing; this PRD requires only compatible tagging and amounts.
- **Policy Engine** enforcement and **resource topology** changes — OSS.
- **Coupon / campaign lifecycle** (creation, distribution, redemption limits, fraud controls) — Promotions; Rating consumes frozen coupon definitions at evaluation time only.
- **Spend control and credit risk** — real-time spend stop / limit enforcement is OSS / Policy Engine; post-aggregation spend caps / bill-shock are Billing; credit risk and prepaid gating are Finance. Rating sets the floor/cap **amount** but performs no enforcement or gating. Launch without a hard spend ceiling requires Finance acceptance (§15).
- **`one_time` charge billing** (T-D-18, amended T-D-36 2026-09-20; reversal proposed — DECISIONS R-19) — not rated: no evaluation unit is synthesized for this `chargeKind` (`one_time_setup` is withdrawn; there are exactly three kinds); Subscriptions/Billing bill it from the price of the subscription's accepted price binding (R-29), with the dedup Subscriptions owns, keyed `(tenantId, subscriptionId, phaseEntryId, componentOccurrenceId, chargeLineId)` (SUB-D-28). One-time bindings still appear in Pricing's `resolve` answer and in order evaluation (T-D-79).

### 5.3 Release Gates

The requirements below keep their priority — changing it is the product owner's decision — but the
launch design does not deliver them. Each stays non-implementable until its gate opens, and until
then Rating fails closed with the reason shown and never charges a default (DESIGN §4.10 capability
matrix).

| Requirement | Priority | Gate (owner decision or contract) | Behaviour until the gate opens |
|---|---|---|---|
| `cpt-cf-bss-rating-fr-level-aggregation` (`Peak` / `TimeWeighted` input folds) | p1 | R-06 (usage-collector / Product) | `unsupported_input_fold`; only `Sum` inputs of a derived usage type are rated |
| `cpt-cf-bss-rating-fr-composite-meter-eval` | p1 | R-31 (a Products typed read of a derived usage declaration) | `derived_declaration_unavailable`; usage children stay pending |
| Every usage and recurring requirement | p1 | Subscriptions carries the period's pins, plan revision, quantity and `BillingTerms` on each fact (SUB-D-29; R-03; UPSTREAM_REQS §2.6) | no fact ⇒ no child; a fact without pins, revision or quantity ⇒ its children wait `pending(pins_unavailable)`; `price_uncovered` for an uncovered cell |
| `cpt-cf-bss-rating-fr-committed-usage`, `cpt-cf-bss-rating-fr-commitment-drawdown` | p1 | R-11 (a Contracts source for commitment pools and balances) | a line that references pools fails closed (`pool_input_torn`) |
| `cpt-cf-bss-rating-fr-customer-contract-overlay` | p1 | R-11 (a Contracts source for contract overrides) | step applies nothing; a referenced contract override fails closed |
| `cpt-cf-bss-rating-fr-reservation-consumption-flavor`, `cpt-cf-bss-rating-fr-capacity-charge` | p1 | R-11 (a reservation-match and reserved-rate source; Pricing defers reserved capacity) | `reservation_match_unavailable` |
| `cpt-cf-bss-rating-fr-dimensional-pricing` (non-empty dimension values) | p1 | R-16 (dimension-key encoding) | empty `dimension_key` only; otherwise `unsupported_primitive` |
| `cpt-cf-bss-rating-fr-multi-currency`, `cpt-cf-bss-rating-fr-fx-policy` | p1 | R-07 (a pinnable FX source) | native currency only; otherwise `fx_not_supported` |
| `cpt-cf-bss-rating-fr-tier-aggregation-window` (`resource` aggregation scope) | p1 | R-21 (scope proofs that list resources) | `resource_scope_not_supported`; `subscription_line` scope only |
| `cpt-cf-bss-rating-fr-coupon-application-order`, `cpt-cf-bss-rating-fr-coupon-stacking` | p2 | R-11 (a Promotions source; Pricing defers promotions, D-409) | `coupon_source_unavailable` |

Closed gates (2026-10-08): `subscription_lifetime` and `per_event` windows do not exist in Pricing (R-28 closed); publish-time validators have no hook in the shared approval engine (R-12 closed). The pricing adapter itself is designed (R-17 closed by T-D-73…T-D-78).

## 6. Functional Requirements

> **Content boundary**: FRs define WHAT must be resolved (posting/evaluation semantics), not data models or APIs. Concrete schemas, proto definitions, error taxonomies, and mathematical formulas with symbol definitions are owned by the design set — [`DESIGN.md`](./DESIGN.md) (its §6 holds the detailed contracts of the former slice documents) and the feature specifications under [`features/`](./features/) (the pre-consolidation `DESIGN-tariffs-pricing-logic-*/` path this line used to cite no longer exists — ADR-0002). The deterministic evaluation order is normative in §17.1 (re-cut 2026-10-08 against the PriceBook model).

### 6.1 Deterministic Evaluation

#### Deterministic evaluation API

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-fr-deterministic-evaluation-api`

Rating evaluation **MUST** expose a conceptual evaluation contract that, for a given evaluation context at timestamp `t` (UTC), prices the bindings Pricing resolved for the subscription (the binding of record) and produces a **resolved price outcome** (bound prices, model kind, tier thresholds, exact amounts) plus a `pricingSnapshotRef` and evaluation metadata. It **MUST** be replay-safe: a replay reads the stored binding of record, never a new `resolve` call.

**Rationale**: Rating and Finance require a stable, reproducible outcome to reproduce charges and audits.

**Actors**: `cpt-cf-bss-rating-actor-rating`

#### Pre-purchase evaluation (order-time price preview)

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-fr-pre-purchase-evaluation`

Rating evaluation **MUST** expose a **pre-purchase** evaluation contract for a prospective purchase that has **no subscription yet**. The request is the one Orders Lifecycle defines (Orders D-154, D-167; DECISIONS T-D-79): an `assessment_id`, a `resolve_date`, the tenant axes, the `market{currency, region}`, an optional `contract_ref`, and `lines[{line_id, plan_revision_id, term: Rolling | FixedPeriods{count}, cycle: month | year, items[{item_id, quantity, chains[{dim_value, binding}]}]}]` — `market`, `contract_ref` and the per-line `term`/`cycle` are Rating's additions pending Orders' confirmation (R-24) of the context Orders lists — the **exact bindings** Pricing resolved for the order, which Rating prices as given (no renewal walk, no `resolve` call). The contract **MUST** return, per item, per line and for the whole order, gross, discount and net amounts **decomposed by `chargeKind`** — `recurring`, `one_time` (listed once) and `usage` flagged as carrying no committed amount (it prices at rating time) — together with the minimum-fee application, the currency, `currency_scale` and rounding policy, and the **net pre-tax TCV** (rolling terms annualised ×12 for `month` and ×1 for `year`; any other cycle is refused). Amounts are returned both exact and as major-unit display decimals rounded HALF_EVEN at `currency_scale` and marked as estimates (the money ADR retires the integer minor units Orders' D-40 still names; DECISIONS T-D-80, R-24). The outcome is **non-authoritative**: it **MUST NOT** be a billing input, **MUST NOT** post, and **MUST NOT** create evaluation state. `discount` and `promotion_status` **MUST** be reported `unavailable` (promotions are deferred in Pricing, D-409), never as a zero discount; brand-scoped terms and indicative tax are likewise unavailable. Access is `sellerTenantId`-scoped. A missing or inconsistent input fails with `evaluation-unavailable` (the code Orders expects).

**Design**: `OrderEvaluationV1::evaluate` (DECISIONS T-D-54, refined by T-D-79 to Orders' D-167 contract; it replaces the Seam Atlas C06 DTO with `plan_id`, `price_ids[]` and `catalog_version`). **Orders ask (R-24)**: Orders Lifecycle requires totals at every submit and amendment and asks to raise this requirement to `p1` and drop its catalog-version prefix (`orders-lifecycle/docs/UPSTREAM_REQS.md:315-319`); the priority stays `p2` until the product owner decides.

**Rationale**: A purchase must be priced, displayed and threshold-checked **before** the subscription that anchors evaluation exists; without this contract every caller would re-implement the money arithmetic and fork it. Selection stays Pricing's (the order carries the bindings); the arithmetic stays single-sourced here.

**Actors**: `cpt-cf-bss-rating-actor-partner-admin`, `cpt-cf-bss-rating-actor-catalog`

#### Single outcome per frozen context (pure-function core)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-fr-single-outcome-determinism`

The determinism contract is stated over the **child window** (steps 1–2): the window-aggregated quantity `Q` for the child's aggregation key and rating window (`BillingCycle` or `CalendarHour`, from the binding's usage rating policy) — for a derived usage type, the sum over UTC hours of the formula applied to each hour's folded inputs (`fr-composite-meter-eval`); for a recurring line, the fact's quantity over the line's slices. Given frozen inputs `(window-aggregated inputs, binding of record, engine generation)`, the monetary outcome **MUST** be identical across replay, recompute, and cross-region batch workers. The windowed `Q` **MUST** be materialized and owned by the **rating pipeline** (Design [contract 13](DESIGN.md#contract-13)); **rating-core** receives `Q` as a frozen input and **MUST NOT** aggregate. Concurrent re-resolution of one child **MUST** serialize on that child.

**Rationale**: A pure-function core over frozen, window-aggregated inputs is what makes replay and late-arrival handling non-divergent without cross-partition locks.

**Actors**: `cpt-cf-bss-rating-actor-rating`

#### Snapshot carry

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-fr-snapshot-carry`

Every evaluation **MUST** emit identifiers sufficient for manifest `BillableItem.pricingSnapshotRef` and stable `{skuId, planId, priceId}`. `pricingSnapshotRef` **MUST** reference every frozen input of the charge listed in §1.4 — the plan revision, the resolve date, the bindings with their price ids and money digests, SKU versions, usage rating policies and invoice inputs, the derived usage declaration, the fact version and the engine generation — composed by Rating from the binding of record it rated with. Pricing keeps no binding and freezes no descriptors on a price (pricing D-389), so this copy is the only reproducible record.

**Rationale**: Reproducibility requires freezing every commercial input; Pricing's `resolve` re-reads SKU versions as of each date and a renewal walk moves with new prices, so only the stored binding reproduces a past result.

**Actors**: `cpt-cf-bss-rating-actor-billing`

#### Usage and correction idempotency

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-fr-idempotency`

Same usage idempotency key + same snapshot **MUST NOT** double-charge (Rating dedup remains authoritative). Corrections from retroactivity (and, once FX exists, a period-FX close) are **new result revisions**, not the original usage key; each revision **MUST** carry a stable correction identity naming its predecessor — `(child_id, window_revision)` for a child (usage window or period-driven line, so a re-rate of a recurring line dedups exactly like a usage correction — Design 01 §4.2) and `(fact_id, result_revision, previous_result_revision)` for the parent, where `previous_result_revision` is absent on revision 1 and **MUST** equal `result_revision − 1` on every later revision (a delivery that breaks this is invalid and quarantined; the consumer need not have received the predecessor, because each revision is complete) — so a re-rate retry is idempotent and cannot produce a second revision for the same inputs. Rating owns revision dedup; Billing owns the dedup of the adjustment it derives (per `invoice_line_key` against its accepted target).

**Design**: Rating emits complete result revisions; Billing derives the monetary difference (DECISIONS T-D-50, proposed). There is no Rating delta event (design contracts [08 §4.2](DESIGN.md#contract-08-4-2), [15 §4.3](DESIGN.md#contract-15-4-3)).

**Rationale**: Deterministic replay and correction safety require distinct, stable idempotency for usage vs correction revisions.

**Actors**: `cpt-cf-bss-rating-actor-rating`

#### Non-negative resolved price

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-fr-non-negative-price`

A resolved per-line price **MUST NOT** go negative; evaluation **MUST** clamp to zero or emit the residual as a structured credit (clamp-vs-credit policy TBD — §15). Applies to the final line amount, after the dormant contract, coupon and FX placeholders (§17.1; a billing-currency `fixed_amount` coupon would be the first input able to drive a line negative, so the guard clamps the post-FX amount — Design contracts 01 §4.4 / 06 / 07), and **before** the minimum-fee floor.

**Rationale**: Negative resolved lines corrupt downstream rating and revenue; a floor must not mask a negative line.

**Actors**: `cpt-cf-bss-rating-actor-finance-fx`

#### Separation from posted financials

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-fr-separation`

Rating evaluation **MUST NOT** mutate Usage or posted invoices; retroactive outcomes **MUST** flow through the Adjustment path (manifest §4.2): Rating publishes a complete new result revision and Billing derives the adjustment. A correcting/negative usage event **MUST** deterministically reverse its prior commercial effect (decrement tier counter `Q` for the affected child window; refill a drawn-down commitment pool once Contracts exists) in that new revision; it **MUST NOT** drive a resolved line negative. Correction ingestion and dedup remain Rating.

**Design**: the "Adjustment path" is a new complete result revision from Rating; the compensating amount is computed by Billing against its accepted target (T-D-50).

**Rationale**: Posted-financial immutability and auditable corrections are manifest invariants.

**Actors**: `cpt-cf-bss-rating-actor-billing`

### 6.2 Pricing Models

> **Model SoR (pricing D-386, adopted verbatim):** an entry's model is one of `{flat, per_unit, graduated, volume, package}`, fixed for the entry's life (pricing D-427). Usage entries take `per_unit`, `graduated`, `volume` or `package`; recurring and one-time entries take `flat` or `per_unit`. Money is Pricing's `PriceModel` in exact major-unit decimals — `Flat{amount}`, `PerUnit{unit_amount}`, `Graduated{tiers}`, `Volume{tiers}`, `Package{package_size, package_price}`, `Tier{up_to (exclusive), rate}` — kept exact in major units, tagged with the binding's `currency_scale`, never scaled or rounded (DECISIONS T-D-74, T-D-80). Bands are half-open and the last band is open (`TIER_TOP_CLOSED` is refused). `hybrid` and `committed usage` are plan-composition / commercial constructs, **not** model values. Package money is readable but not saleable in a new sale (pricing D-504).

#### Flat pricing

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-fr-flat-pricing`

A flat recurring or one-time charge **MUST** be the price's `amount` per billing period (prorated by coverage, `fr-mid-cycle-proration`); no thresholds are evaluated. Pricing allows no flat usage entry (pricing D-386).

**Rationale**: The base model must be unambiguous and threshold-free.

**Actors**: `cpt-cf-bss-rating-actor-rating`

#### Per-unit (per-seat) pricing

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-fr-per-unit-pricing`

A per-unit charge **MUST** be `unit_amount × quantity`. For a recurring line the quantity is the period's committed quantity, which Subscriptions carries on the fact (its effective-dated `QuantityInterval`; the first period from the accepted order line's `quantity`) — **never** metered usage `Q`. For a usage line the quantity is the window's billable `Q`. *Superseded (2026-10-08)*: the plan-level `quantitySource` (`subscription_seat_count` / `manual`) does not exist in Pricing.

**Rationale**: Per-seat plans are a launch model; the seat count is a Subscriptions input, not usage, and no Pricing read carries it.

**Actors**: `cpt-cf-bss-rating-actor-subscriptions`

#### Tiered (graduated) pricing

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-fr-tiered-graduated`

With two or more tiers, each unit **MUST** be charged at its marginal band rate; a single tier rate **MUST NOT** be applied to all units. With one tier only, graduated and volume are numerically identical — the distinction is by the entry's model, not by this rule. Tier counter `Q` **MUST** accumulate over the binding's rating window (`fr-tier-aggregation-window`).

**Rationale**: Graduated vs volume must be fixed in writing to prevent rating divergence.

**Actors**: `cpt-cf-bss-rating-actor-rating`

#### Volume Variant A (rate on total Q)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-fr-volume-variant-a`

A single tier rate **MUST** apply to **all** units, chosen by the total quantity `Q` within the rating window (the rate of the first band with `Q < up_to`, pricing's `amount_for`); the model is the entry's `volume`, distinguishable from graduated.

**Rationale**: Whole-quantity pricing is a distinct commercial model and must be explicit.

**Actors**: `cpt-cf-bss-rating-actor-rating`

#### Package (block) pricing

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-fr-package-pricing`

A package (block) usage charge **MUST** be `ceil(Q / package_size) × package_price` over the rating window — whole blocks are billed and a partial block rounds **up** to one block. This is the entry model `package` (usage-only, pricing D-386); `package_size` and `package_price` come from the binding of record. Distinct from volume (a single per-unit rate on total `Q`). **Volume Variant B (per-tier flat block fee) is not authorable.** Package money is readable but a new sale refuses it (pricing D-504), so a package price reaches Rating only on an existing subscription.

**Rationale**: Block pricing (e.g. per 1000 API calls) is a distinct construct with round-up-to-block math; per-tier block fees have no authoring home in Pricing.

**Actors**: `cpt-cf-bss-rating-actor-rating`

#### Level-based (gauge) aggregation

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-fr-level-aggregation`

> **Suspended (R-06).** Until R-06 is decided, a derived usage type whose inputs fold `Peak` or `TimeWeighted` fails closed (`unsupported_input_fold`) and nothing below is applied at launch; the fold is the target rule once R-06 lands (consistent with §5.1, §17.1 and DESIGN contract 03).

A level-shaped meter is a **derived usage type input** whose declared `GranuleFold` is `Peak` or `TimeWeighted` (products P-D-229; T-D-17 amended by T-D-39); its records are point-stamped **gauge samples** in the input's unit. Rating **MUST** fold each such input per **UTC hour** (the only `Granularity` Products declares): `Peak` → the maximum sample in the hour (a partial hour bills as a full one); `TimeWeighted` → the step-function integral of the level over the hour, the last level held for at most the input's `max_hold_seconds` (≤ 86 400), beyond which the level reads **0** and an operator signal **MUST** raise — never a guessed value. The derived formula then applies to each hour's folded inputs, and the window quantity **MUST** equal the **sum of the hourly outputs**, which keeps `Q` additive: the child's counter key, band math, complete-revision corrections apply to it **unchanged**. A late or corrected sample **MUST** re-fold its hour and, for `TimeWeighted`, the following hours (up to `max_hold_seconds`) whose held level it changes. *Superseded (2026-10-08)*: the price-row `aggregationFunction` / `aggregationGranularity ∈ {hour, day}` / `maxHold` (pricing D-44) — the usage rating policy's own `fold` is `SUM` only (pricing D-514).

**Rationale**: The launch product set bills on levels — cloudlet peak-per-hour and storage GB-month — and the commercial rule (which fold, which cadence) must live in a declaration (Products' derived usage type), not be pre-folded inside the emitting source (which would hide raw levels from audit and make retro re-aggregation impossible). Summing hourly outputs keeps `Q` additive so no counter invariant is disturbed (T-D-17, T-D-39).

> **Open conflict**: the usage-collector PRD forbids charging from non-`SUM` folds and requires emitter pre-integration (DECISIONS R-06, §15).

**Actors**: `cpt-cf-bss-rating-actor-rating`, `cpt-cf-bss-rating-actor-oss-metering`

#### Hybrid pricing

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-fr-hybrid-pricing`

Recurring and usage components **MUST** be emitted as **two distinct lines** under one plan (so Billing can itemize) — they are distinct plan items and entries, each evaluated per its own binding and period boundaries. A hybrid "minimum commitment" **MUST** be expressed as committed usage (release-gated, §5.3); a price's minimum fee is a separate per-price construct (§6.11) and **MUST NOT** be conflated. Coupon attachment (`applyScope`) is dormant with Promotions (pricing D-409).

**Rationale**: Itemization and min-commit disambiguation are required for auditable hybrid plans.

**Actors**: `cpt-cf-bss-rating-actor-billing`

#### Committed usage

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-fr-committed-usage`

> **Release gate (R-11)**: not in the launch design; fails closed until the gate opens (§5.3).

In-commitment and overage portions **MUST** be charged at distinct rates over the ordered `commitmentPools[]` (the commitments slot of step 4; dormant, R-11). In-commit **billability** follows the pool's frozen `poolType`: `prepaid_drawdown` (pool billed upfront at sale, outside rating-core; in-commit consumption emits a due-zero line with notional lineage) or `committed_rate` (in-commit bills in arrears at the committed rate). Period true-up follows the contract and **MUST** be surfaced as a structured `TrueUpObligation` (amount, period, contract ref) for Billing — not an implicit posted charge; for `committed_rate` pools the shortfall formula is normative (quantity basis: unmet committed quantity × committed rate; spend basis: unmet committed spend), and `prepaid_drawdown` pools surface no pool-driven true-up (unused balance follows the rollover policy). A correcting/negative usage event **MUST** refill the drawn-down pool in a complete new result revision (Billing derives the compensating amount, T-D-50); it **MUST NOT** drive the resolved line negative.

**Rationale**: Prepaid/overage economics and true-ups must be explicit and reversible.

**Actors**: `cpt-cf-bss-rating-actor-contracts`

### 6.3 Rule Evaluation Order

> The normative evaluation order is §17.1 (re-cut 2026-10-08: bind, meter, model formula, minimum-fee floor, guards; contract, commitment, coupon and FX placeholders dormant). The FRs below carry the requirements that the order enforces.

#### Deterministic evaluation order

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-fr-evaluation-order`

For any evaluation at `t` (UTC) and context `ctx`, the engine **MUST** apply the fixed order of §17.1: (1) **bind** — take the binding of record (Pricing's `resolve` answer for the period's pins; slices at each `ends_on`), (2) **meter** — the window quantity, from the derived usage type for usage, (3) **model formula**, (4) the dormant contract, commitment, coupon and FX placeholders (fail closed if referenced), (5) **minimum-fee floor** at parent roll-up, (6) **guards and emit**. The step order is **invariant** — there is **no reordering knob**. Replay over identical inputs **MUST** be byte-identical. *Superseded (2026-10-08)*: the nine-step order with phase resolution, base-row selection and price overlays.

**Rationale**: A single invariant order is the basis of determinism (AC 1).

**Actors**: `cpt-cf-bss-rating-actor-rating`

#### Base price binding (selection superseded)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-fr-base-catalog-selection`

**Superseded (2026-10-08)**: Rating no longer selects a base row. **Pricing selects**: `resolve(revision_id, date, item_id?, pins)` binds, per item and dimension value, the price in force for a signup (own value chain first, else the default chain) and, for a renewal, walks the pinned chain through `all` successors and stops before the first `new` one (pricing D-420). Rating **MUST** price exactly the binding of record: the bindings `resolve` returned for the fact's plan revision and pins on the period start, and again on each binding's `ends_on` inside the period (pricing D-397, D-425; DECISIONS T-D-73). An **uncovered** cell **MUST** fail explicitly (`price_uncovered`) — never a zero or a guessed price. The fact's accepted binding **MUST** equal the resolved binding per item and value, else fail closed (`binding_mismatch`). When invoice currency equals the book's currency, FX is skipped.

**Rationale**: One selector (Pricing) and one stored binding prevent silent mispricing and divergence from the owner's renewal rules.

**Actors**: `cpt-cf-bss-rating-actor-catalog`

#### Meter mapping and billing granularity

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-fr-meter-mapping-granularity`

Step 2 **MUST** map usage to a charge line keyed by `(skuId, dimensionKey)`: the binding's `meter` names the Products derived usage type the SKU sells (`MeterRef{usage_type_id: products.derived/<code>@<n>, version}`; products P-D-259), whose inputs are raw collector usage types. Pricing refuses a plan with two items on one meter (`METER_DUPLICATE`), and Rating **MUST** fail closed if two charge lines of one plan revision and charge kind claim the same `(skuId, dimensionKey)`. Any rounding of the quantity **MUST** apply to the aggregated measure, **never per raw usage record** (twelve 5-minute records in one hour count as one hour of input, not twelve). The merge/aggregation is owned by the **rating pipeline** ([contract 13](DESIGN.md#contract-13)); **rating-core** evaluates the derived formula and prices the aggregate.

**Rationale**: One charge line per meter and aggregate-level rounding prevent line collisions and ephemeral over-charge.

**Actors**: `cpt-cf-bss-rating-actor-rating`

#### PriceOverlay scope → tenant-axis mapping (superseded)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-fr-priceoverlay-scope-mapping`

> **Superseded (2026-10-08)**: Pricing has no price overlays (PriceBook spec item 2; DECISIONS T-D-02 obsolete). A partner's or customer group's prices are a separate price book or SKU, which Pricing binds through the plan revision the subscription holds; no scope-to-tenant-axis matching remains in Rating. The tenant-isolation intent survives: a binding is read under the seller tenant's scope (pricing D-424) and a contract override of one payer **MUST NOT** apply to another (`fr-customer-contract-overlay`).

**Rationale**: Kept for traceability; cross-tenant isolation is now enforced by tenant-scoped bindings (AC 13).

**Actors**: `cpt-cf-bss-rating-actor-oss-ams`

### 6.4 Override Hierarchy (overlays superseded)

#### Overlay stacking (partner / OrgTier / brand / region) (superseded)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-fr-overlay-stacking`

> **Superseded (2026-10-08)**: Pricing has no price overlays, so there is no partner / OrgTier / brand / region stack to apply (PriceBook spec item 2; DECISIONS T-D-02, T-D-30 obsolete). Rating's evaluation order has no overlay step (§17.1).

**Rationale**: Kept for traceability (AC 2 superseded).

**Actors**: `cpt-cf-bss-rating-actor-catalog`

#### Customer / contract overlay

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-fr-customer-contract-overlay`

> **Release gate (R-11)**: not in the launch design; fails closed until the gate opens (§5.3).

When a Contracts source exists, contract/account-level overrides **MUST** apply to the bound price as the first dormant placeholder of §17.1, bounded by entitlement and approval rules; contract terms outrank the plan's price book. Overrides **MUST NOT** introduce metering dimensions absent from the bound entry; contract publish validation **MUST** reject such overrides (fail-closed). Customer-layer changes **MUST NOT** silently weaken audit controls (Contract workflow + optional Finance approval).

**Rationale**: Contract precedence and dimension integrity must hold without weakening controls.

**Actors**: `cpt-cf-bss-rating-actor-contracts`

#### Bounded composition (anti-drift cap) (superseded)

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-fr-bounded-composition-cap`

> **Superseded (2026-10-08)**: the anti-drift cap bounded markup across a chain of price overlays, and Pricing has no overlays. A cap on a contract override, if Contracts defines one, is part of `fr-customer-contract-overlay`.

**Rationale**: Kept for traceability.

**Actors**: `cpt-cf-bss-rating-actor-contracts`

### 6.5 Rating Window, Eligibility, Phases, Granularity

#### Rating window and aggregation

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-fr-tier-aggregation-window`

Tiered/volume models **MUST** accumulate tier counter `Q` over the rating window of the binding's immutable usage rating policy (pricing D-502, D-514; DECISIONS T-D-75): `rating_window = BillingCycle` — the fact's billing period; `CalendarHour` — a UTC clock hour. Tiers restart at each window start (`reset = rating_window_start`). A clipped first or last window rates its actual quantity against whole thresholds (`partial_window = actual_quantity_full_thresholds`). The window quantity is the sum of its quantities (`fold = SUM`). The policy's `aggregation_scope` sets whether `Q` pools per subscription line or per resource. Different entries of one plan **MAY** have different windows; there is no plan-wide window and no aggregation across subscriptions (pricing D-503, D-509). The policy (`policy_id`, `version`, `digest`) **MUST** be recorded in evaluation metadata and in Rating's snapshot (`rsnap1`).

> **Design**: `resource` aggregation scope is LAUNCH-GATED until scope proofs list resources (`resource_scope_not_supported`; R-21). *Superseded (2026-10-08)*: `tierAggregationWindow` and its values `calendar_month`, `invoice_period`, `subscription_lifetime`, `per_event` and `per_hour`, and the `billingAnchorPolicy` anchor — periods and anchors come from Subscriptions' facts (R-28 closed).

**Rationale**: The tier-counter reset policy is commercially significant and is Pricing's, frozen per entry (AC 12).

**Actors**: `cpt-cf-bss-rating-actor-rating`

#### Price eligibility and grandfathering (superseded)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-fr-price-eligibility-grandfathering`

> **Superseded (2026-10-08)**: eligibility classes and cohort generations no longer exist (pricing D-397: consumer pins replace cohorts and catalog versions). A price's `eligibility: all | new` drives Pricing's renewal walk: a pinned subscription takes an `all` successor and stays before a `new` one, and Pricing marks the price it stays on (`keep_for_bound`; pricing D-420). Rating **MUST** price the binding of record and **MUST NOT** re-apply eligibility; an uncovered cell fails closed (`price_uncovered`).

**Rationale**: New-vs-existing pricing remains first-class but is Pricing's renewal walk (AC 14).

**Actors**: `cpt-cf-bss-rating-actor-subscriptions`

#### Plan phases (superseded)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-fr-plan-phases`

> **Superseded (2026-10-08)**: Pricing has no plan phases or trials (pricing PRD; a new sale refuses phases, D-504), so Rating resolves no active phase. A change of price within a subscription arrives as new pins on a new fact version (SUB-D-29). Subscriptions still deduplicates one-time charges per phase-entry occurrence (SUB-D-28).

**Rationale**: Kept for traceability (AC 14 superseded).

**Actors**: `cpt-cf-bss-rating-actor-subscriptions`

#### Billing granularity round-up

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-fr-billing-granularity`

Any rounding of a usage quantity **MUST** apply to the **merged/aggregated** measure (not per raw record) and be recorded in evaluation metadata. Pricing on `main` publishes no billing-granularity field; the only rounding of a usage quantity is a derived usage type's own `output_scale` / `output_round`, applied per hour by `bss_products_sdk::derived::evaluate` (products P-D-229). A per-resource `minimumCharge` MAY be configured to bound ephemeral-resource over-charge (§15). *Superseded (2026-10-08)*: the price-row `billingGranularity` enum.

**Rationale**: Rounding of quantities must be deterministic and applied at the aggregate (AC 15).

**Actors**: `cpt-cf-bss-rating-actor-rating`

### 6.6 Commitments and Reservations

#### Commitment drawdown, overage, true-up

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-fr-commitment-drawdown`

> **Release gate (R-11)**: not in the launch design; fails closed until the gate opens (§5.3).

The commitments slot of step 4 (dormant, R-11) **MUST** apply drawdown/overage per contract over an ordered list of commitment pools (`commitmentPools[]`, Contracts SoR), in declared order (waterfall): each pool absorbs quantity/spend up to its remaining balance before the next; residual beyond all pools is overage / on-demand. A single pool is the default special case. **How the residual is priced is selected by the frozen `overageRate` field (T-D-19, retained for when a Contracts source exists):** present on the `commitmentReservation` segment ⇒ the residual bills at that **flat frozen rate** (no band math on the residual); absent ⇒ the residual bills at the **banded on-demand rates**, placed over the **post-reservation remainder `Q`** — the reservation exclusion of §6.6 applies first (matched quantity removed, remainder banded from zero), and pool draws then **never** reduce the counter (the pool-vs-reservation asymmetry). With no reservation on the line the remainder is the full `Q`. The selector is the field's **presence**, frozen at contract publish; Rating consumes the rate values and never decides them. The frozen pool set, per-pool balances, draw order, rollover policy, and any reserved-vs-pool split **MUST** be carried in `pricingSnapshotRef`. Commitment is **always** evaluated at step 6 (no reordering).

**Rationale**: Deterministic waterfall drawdown with frozen pool state is required for reproducible committed-usage billing.

**Actors**: `cpt-cf-bss-rating-actor-contracts`

#### Reservation pricing — consumption-flavor

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-fr-reservation-consumption-flavor`

> **Release gate (R-11)**: not in the launch design; fails closed until the gate opens (§5.3).

When a consumption-flavor `reservationMatch` is present, the **matched portion** of measured usage **MUST** be priced at the **reserved rate** — reserved rates have no source today (Pricing defers reserved capacity; negotiated rates would come from Contracts) — and the remainder at the bound on-demand price. The reserved portion **MUST** be excluded from `commitmentPools[]` drawdown (reservation precedes pools) **and from the on-demand tier counter `Q`** — the remainder re-bands from zero (pricing `inst-rv-tier-q`); the in-commit pool quantity is **NOT** excluded (pool-vs-reservation asymmetry). The reservation-match identifier **MUST** be recorded in metadata and `pricingSnapshotRef`. With no `reservationMatch`, evaluation prices as pure usage.

**Rationale**: Reserved-rate coverage of measured usage (RI-style) must be deterministic and pool-precedent (AC 19).

**Actors**: `cpt-cf-bss-rating-actor-contracts`

#### Provisioned-capacity charging — capacity-flavor

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-fr-capacity-charge`

> **Release gate (R-11)**: not in the launch design; fails closed until the gate opens (§5.3).

When a capacity-flavor `reservationMatch` with `reservedQuantity` is present, evaluation **MUST** emit a `capacityCharge` = reserved rate x `reservedQuantity`, **regardless of measured usage** (zero usage still bills the allocation). The `capacityCharge` **MUST NOT** be reduced by absent usage and **MUST NOT** draw down `commitmentPools[]`. `reservedQuantity`, reserved rate, and flavor **MUST** be frozen in `pricingSnapshotRef`.

**Rationale**: Provisioned disks/IOPS bill on allocation, not consumption (AC 20).

**Actors**: `cpt-cf-bss-rating-actor-contracts`

### 6.7 Dimensional (Cloud) Pricing

#### Dimensional pricing — `(skuId, dimensionKey)` lines

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-fr-dimensional-pricing`

> **Release gate (R-16)**: not in the launch design; fails closed until the gate opens (§5.3).

Each distinct `(skuId, dimension value)` (e.g. a `region` value) **MUST** resolve to its own charge line and price, with no line collision: an entry has at most one dimension key and each value has its own price chain, with the null value as the default chain (pricing D-385); Pricing's `resolve` binds a value's own chain before the default (`via_default`). The bound dimension key and value **MUST** be recorded in the binding of record. A plan whose entry declares no dimension prices as a single line. A record arriving with empty or partial dimension values on a dimension-declaring entry **MUST NOT** be silently priced as a single line; evaluation **MUST** route it to the default chain only when Pricing bound it there, or fail-closed (reject/quarantine) — never guess.

**Rationale**: Real IaaS catalogs require per-dimension pricing without collapsing or guessing dimensions (AC 18).

**Actors**: `cpt-cf-bss-rating-actor-rating`

#### Usage dimension-population contract (BSS side)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-fr-dimension-population-contract`

Pricing **declares** one dimension key per entry and binds a price per value (pricing D-385); Rating **records** the bound key and value in the binding of record and passes the usage record's dimension value through to the charge line. Declaration authoring is Pricing's — Rating owns only the record. Dimension **value pricing** stays OSS-emission-gated. Value **emission** on usage is OSS metering (external upstream requirement). Until OSS emits dimension values, only the default chain is usable and per-combination SKUs are the only workaround (exploding cardinality — tracked as critical-path risk, §16).

**Rationale**: The BSS side of the dimension contract is closeable now; the OSS emission is the gating critical path.

**Actors**: `cpt-cf-bss-rating-actor-oss-metering`

#### Composite (derived) meter evaluation

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-fr-composite-meter-eval`

> **Release gate (R-31)**: needs a Products typed read of a stored derived usage declaration (§5.3).

A usage SKU sells a **derived usage type**: Products declares it as versioned data — inputs (raw collector usage types, each with its `GranuleFold`), a formula, the UTC-hour granularity, the output unit and its rounding — and the SKU's meter names it as `products.derived/<code>@<n>` (products P-D-229, P-D-259; ≥ 1 input, `MIN_INPUTS = 1`). Rating **MUST** fold each input over each UTC hour, evaluate the declared formula for the hour with `bss_products_sdk::derived::evaluate` (the one implementation Products validates with), and sum the hourly outputs into the window quantity, which it then prices by the entry's model (DECISIONS T-D-39, T-D-76). The declaration and its digest **MUST** be frozen in the binding of record; Rating **MUST NOT** author or mutate the derivation. At launch only `Sum` inputs are rated (`fr-level-aggregation`, R-06).

**Rationale**: A real VM line (vCPU + RAM as one priced unit) requires evaluating a declared composite formula deterministically; Products provides the declaration and its evaluator.

**Actors**: `cpt-cf-bss-rating-actor-rating`

### 6.8 Coupons (Promotions Overlay)

> Coupon entity lifecycle and campaign management are owned by Promotions — on `main` a Pricing concern, deferred (pricing D-409); resolve returns no promotion. Rating owns deterministic application semantics for when a source exists. Full placement/stacking/consumption contract preserved in §17.2.

#### Coupon application order

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-fr-coupon-application-order`

Coupons adjust the bound price, applied at the coupon placeholder of §17.1 after the contract and commitment placeholders (post-commitment line amount). Default: `settlementCurrency = price` coupons apply in price currency before FX; `settlementCurrency = billing` coupons apply after FX on the billing-currency amount (same `fxTableVersion`). The applied coupon id(s) and pre-/post-discount amounts **MUST** be recorded in metadata.

**Rationale**: Deterministic coupon placement relative to the contract override, commitments, and FX is required to reproduce charges (AC 16).

**Actors**: `cpt-cf-bss-rating-actor-promotions`

#### Coupon stacking and conflicts

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-fr-coupon-stacking`

Default stacking is `exclusive_best` (select the single coupon yielding the largest customer benefit; others MUST NOT apply on the same line). **Cross-scope exclusivity (T-D-20):** when the candidate set contains a `line_total`-scoped coupon, the exclusivity set **widens to the plan** — exactly one coupon applies **per plan per period**, the winner being the candidate with the greatest total benefit measured on its own scope; without this, a `usage`-scoped and a `line_total`-scoped coupon are "not on the same line" and both apply, i.e. double discounting under the default policy. `ordered_stack` applies only when a Promotions campaign explicitly links coupons with `stackSequence` (ascending; each step uses the prior output). Campaign-marked incompatible pairs **MUST** fail-closed at redemption bind time if both would apply. A coupon snapshot omitting `applyScope` (or `stackSequence` under `ordered_stack`) **MUST** fail-closed — Rating **MUST NOT** infer it.

**Launch limitation (T-D-22):** `applyScope = line_total` is **not evaluable at launch** and **MUST** fail closed. The coupon placeholder runs per line and every evaluation unit is sub-plan (a hybrid plan's usage and recurring lines are distinct children on distinct triggers), so no point in the pipeline holds a plan's line set at once — which both `line_total` attachment and T-D-20's cross-scope comparison require. A `line_total` coupon **MUST NOT** be applied to a single line, split against a partially-rated plan, or downgraded to another scope. Defining the plan-period assembly point, its trigger, and its cascade behaviour is a named joint gate with Promotions (§15); T-D-20 is inert until then.

**Rationale**: Winner-takes vs ordered stacking must be unambiguous and fail-closed on missing policy (AC 16).

**Actors**: `cpt-cf-bss-rating-actor-promotions`

### 6.9 Multi-Currency and FX

#### Multi-currency separation

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-fr-multi-currency`

> **Release gate (R-07)**: not in the launch design; fails closed until the gate opens (§5.3).

The engine **MUST** separate **price currency** (the bound price's book currency; a price book has exactly one currency, so per-market prices are separate books — first-class, not FX-derived), **billing currency** (invoice currency per payer account/contract), and **presentment currency** (portal display FX, non-authoritative and outside rating-core — such amounts MUST be labelled estimates). Conversion applies only when billing currency != price currency.

**Rationale**: Conflating list price, settlement currency, and display FX causes disputes.

**Actors**: `cpt-cf-bss-rating-actor-finance-fx`

#### FX policy (rating-core abstraction)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-fr-fx-policy`

> **Release gate (R-07)**: not in the launch design; fails closed until the gate opens (§5.3).

When invoice currency != the bound price's currency, rating-core **MUST** apply the FX table per Finance policy and record `fxTableVersion` or locked-rate id; it **MUST NOT** use implicit/provider-default FX without a policy record. Two deterministic policies: (a) **per-window rate-lock** — final at event time; (b) **invoice-period FX** — a **provisional** amount at the locked/spot rate (flagged provisional, never delivered as final) on the hot path, and a final result at the close-time rate; a later rate correction is a new complete result revision from which Billing derives the adjustment (close-time `fxTableVersion` is authoritative). Replay over identical inputs (including which `fxTableVersion` applied at which stage) **MUST** be byte-identical.

**Design**: FX is dormant (native currency only, R-07); when enabled, invoice-period FX finalizes a child only with the close-time rate, and a later rate correction is a new revision ([contract 07](DESIGN.md#contract-07)).

**Rationale**: Explicit, recorded FX with a provisional estimate and a close-time final revision keeps the hot path fast and replay byte-identical (AC 8).

**Actors**: `cpt-cf-bss-rating-actor-finance-fx`

### 6.10 Retroactivity and Corrections

#### Posted-period protection

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-fr-posted-period-protection`

When Billing has posted the period, a retroactive price change to usage in that period **MUST NOT** alter posted invoice lines; Rating **MUST** publish a complete new result revision naming its predecessor, from which Billing derives the adjustment per immutability rules. Retroactive runs **MUST** separately record usage-observation time and pricing-policy decision time in the audit log. Pricing cannot change an approved price retroactively (approved prices are immutable; a new price cannot start in the past, `WINDOW_START_IN_PAST`), so a retroactive price arrives only as new pins on a new fact version or through an administrative re-rate.

**Design**: Rating does not read period state; Billing derives the adjustment against the posted target (DECISIONS T-D-50, proposed; Billing to confirm).

**Rationale**: Posted financials are immutable; corrections flow as auditable revisions and Billing-derived adjustments (AC 9).

**Actors**: `cpt-cf-bss-rating-actor-billing`

#### Late-arriving usage into an aggregate window

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-fr-late-arriving-usage-reresolve`

For a graduated/volume/package model, late usage arriving after some of the window was rated **MUST** trigger deterministic re-resolution of tier placement for the whole window-aggregated `Q` and a **complete new result revision** for the window (no mutation of prior outputs; Billing derives any adjustment), re-priced **strictly with the window's binding of record** — no new `resolve` call (DECISIONS T-D-73). When a binding's `ends_on` splits the window into slices, each slice prices its own bound price, coupled to earlier slices only via the frozen band offset. The **one sanctioned exception (T-D-21)** is the **administrative re-rate**, which resolves again with the fact's current pins or moves to a new engine generation and replaces the binding of record; every *input* correction stays strictly on it. Whether the revision replaces an open draft or follows posted-period protection is Billing's decision on its own period state; Rating does not read period state.

**Design**: "re-priced strictly with the binding of record" is DECISIONS T-D-42 refined by T-D-73; `periodState` is not read by Rating, so it can never be missing at Rating — the posted/open branch is applied by Billing to the new revision (T-D-50).

**Rationale**: Open-window late arrivals must re-resolve deterministically without mutating prior outputs (AC 10).

**Actors**: `cpt-cf-bss-rating-actor-rating`

#### Usage corrections / negative quantity

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-fr-usage-corrections`

A correcting/negative usage event **MUST** deterministically reverse its prior commercial effect: decrement tier counter `Q` for the affected child window (and, once Contracts exists, refill a drawn-down commitment pool), published as a complete new result revision (Billing derives the compensating amount). It **MUST NOT** drive a resolved line negative. Correction ingestion and dedup remain Rating.

**Rationale**: Reversals must be deterministic and non-negative to keep commitment and tier state correct.

**Actors**: `cpt-cf-bss-rating-actor-rating`

### 6.11 Period-Level and Plan-Change Obligations

> The minimum-fee floor is Rating's step at parent roll-up (§17.1 step 5); other period-level floors and caps have no source today. Full boundary contracts in §17.2.

#### Period-level floor/cap obligation

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-fr-period-floor-cap-obligation`

A price's **minimum fee** **MUST** be applied by Rating at parent roll-up, per price, per subscription, per billing period, before promotions (pricing D-388, D-415; DECISIONS T-D-38): billed amount = `max(sum of the exact amounts of every slice and dimension value rated by that price, minimum_fee × covered fraction)`, the difference emitted as a separate exact `min_fee_topup` line with its lineage (DECISIONS T-D-78). Two values rated 10 + 10 on one default price with `minimum_fee` 30 bill 30; two own prices with `minimum_fee` 30 each bill 60 (pricing D-415). Only `BillingCycle` + `subscription_line` entries carry a minimum fee; Pricing refuses one on a `CalendarHour` or `resource`-scoped entry (pricing D-503, D-504). There is no included quantity to deduct first (pricing D-467). rating-core **MUST NOT** round. The non-negative guard applies to each line **before** the floor; a floor **MUST NOT** mask a negative line. Whether a contractual floor claws back coupon discount is unresolved (§15). A plan-level floor or a period cap has no source: Pricing keeps no plan cap or minimum (pricing D-388), so a `PeriodFloorCapObligation` is emitted only if Contracts defines one.

**Rationale**: A price's minimum fee is per price and per period, so it belongs at roll-up over all slices and values rated by that price, never per line.

**Actors**: `cpt-cf-bss-rating-actor-billing`

#### Mid-cycle proration

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-fr-mid-cycle-proration`

When a binding ends inside a billing period (its `ends_on` — a temporary price's end or an explicit close), charges **MUST** be computed separately for each slice — before `ends_on` with the current binding and from `ends_on` with the binding `resolve` returns on that date for the same pins — each emitted at **full precision** (no invoice rounding); Billing aggregates and rounds. A successor price's start (`effective_to`) is **never** a slice point (pricing D-425). A recurring component prorated across the boundary **MUST** use the covered fraction of DECISIONS T-D-77 (covered UTC seconds / period UTC seconds, exact; confirmation R-30). *Superseded (2026-10-08)*: `PriceWindow` activation and `prorationBasis`.

**Rationale**: Mid-cycle price ends must split deterministically and defer rounding to Billing (AC 7).

**Actors**: `cpt-cf-bss-rating-actor-billing`

#### Plan-change proration

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-fr-plan-change-proration`

On a plan change at `changeEffectiveAt`, evaluation **MUST** rate the old plan over `[periodStart, changeEffectiveAt)` and the new plan over `[changeEffectiveAt, periodEnd)` (half-open, UTC), each with its own plan revision, pins and binding of record, each at full precision (Billing aggregates). The recurring component **MUST** be prorated by the covered fraction of DECISIONS T-D-77. Tier `Q` continuity across the boundary inside one rating window **MUST** follow the continuation key of DECISIONS T-D-36: the subscription, the priced `skuId` / meter / dimension and a compatible window identity continue `Q`, and no price id may reset it; an incompatible usage rating policy is a new entry whose change Subscriptions schedules at the next UTC hour boundary (pricing D-510 E4). Corrections to an already-rated portion **MUST** be published as complete new result revisions (Billing derives the difference, T-D-50). Evaluation **MUST** consume `(changeEffectiveAt, changeMode)` and **MUST NOT** decide the change mode (Subscriptions owns the policy). *Superseded (2026-10-08)*: `prorationBasis` and `usageCounterOnPlanChange` (T-D-29).

**Rationale**: Plan-change splits must be deterministic and consume — not decide — the change mode (AC 17).

**Actors**: `cpt-cf-bss-rating-actor-subscriptions`

### 6.12 Governance and ASC 606 Traceability

#### ASC 606 traceable identifiers

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-fr-asc606-traceable-identifiers`

The resolved price outcome **MUST** always include `performanceObligationRef` and `sspSnapshotPointer` fields (nullable when not applicable). Non-null values **MUST** be immutable once emitted — a later price or SKU change **MUST NOT** alter an emitted reference. Billing/Finance MAY ignore null fields. The GL code, tax category and invoice-line template come from the binding's invoice inputs (`InvoiceInputs`, with their source: entry, SKU version or seller settings; pricing D-421) and are frozen in the binding of record.

**Rationale**: Downstream revenue allocation requires stable, immutable PO/SSP references — not a recognition engine here (AC 10).

**Actors**: `cpt-cf-bss-rating-actor-billing`

#### Publish approval and audit governance (superseded)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-fr-publish-approval-governance`

> **Superseded (2026-10-08)**: approval of prices and plan revisions is Pricing's, through the shared `bss-approval` engine (`gears/bss/libs/approval`; quorum, separation of duties, generation-checked votes) and the `bss-approvals` inbox; Rating has no role there and no validator registration point exists (DECISIONS R-12 closed). Pricing runs its own publish checks (`METER_DUPLICATE`, `ITEM_UNCOVERED`, `FREQUENCY_MIXED`, `TIER_TOP_CLOSED`, …). Rating keeps the equivalent checks at rating time as fail-closed errors and **MUST** audit its own operator actions (DESIGN §4.8).

**Rationale**: Segregation of duties on publish is guaranteed by Pricing's approval engine; Rating's fail-closed checks remain the runtime safety net.

**Actors**: `cpt-cf-bss-rating-actor-platform-operator`

## 7. Non-Functional Requirements

### 7.1 NFR Inclusions

> Targets below are **working assumptions** (baselines from `PRD-metering-pricing-module-202601120119`) pending the program NFR workshop; rows marked TBD MUST be committed before Design lock (§15).

#### Throughput and latency

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-nfr-throughput-latency`

Rating evaluation **MUST** meet p95 latency targets: **< 100 ms** for a price-binding lookup (the stored binding of record, or one `resolve` call when a period opens) and **< 1 s** for the overall rating path; hot-path throughput **MUST** sustain **>= 10M events/day/region**.

**Threshold**: p95 <= 100 ms binding lookup; p95 < 1 s overall rating path; >= 10M events/day/region (working assumption; final acceptance at NFR workshop, date TBD).

**Design**: "overall rating path" is measured Rating-internal (input change → result committed); end-to-end time to a final result is bounded by the finalization delay and evidence arrival (DECISIONS R-13, DESIGN §4.9).

**Rationale**: Rating is on the monetization critical path; delays become revenue leakage or disputes.

#### Horizontal scale (no cross-partition locks)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-nfr-horizontal-scale`

Horizontal scaling **MUST** avoid cross-partition locks on the evaluation hot path; ordering is per child window (aggregation key and rating window), not global.

**Threshold**: Zero cross-partition locks on the hot path; per-partition ordering only.

**Rationale**: Cross-partition locking caps throughput and breaks the >= 10M events/day/region target.

#### Audit completeness and segregation of duties

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-nfr-audit-segregation`

Material price and plan publishes run through Pricing's multi-approver workflow on the shared `bss-approval` engine (separation of duties, quorum, generation-checked votes); Rating does not run a second workflow. Every Rating result **MUST** reference the binding of record it rated with, and every Rating operator action that can change a charge (retry, release, re-rate, resolve, policy writes) **MUST** be authorized and emit an auditable event with actor, subject and request id.

**Threshold**: 100% of material publishes carry Pricing's approval-unit sign-off; 100% of Rating results reference a stored binding of record; 100% of Rating's charge-affecting operator actions are audited.

**Rationale**: CFO-grade controls and partner trust require segregation of duties and complete audit.

#### Resilience (fail-safe, idempotent retries)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-nfr-resilience`

When evaluation cannot read a consistent binding (Pricing unavailable, an incomplete commercial input, an uncovered cell), it **MUST** fail safe (no partial pricing); retries **MUST** be idempotent.

**Threshold**: Zero partial/best-guess priced outputs under read-model lag; idempotent retry on transient failure.

**Rationale**: Financial correctness requires fail-closed behavior, never best-guess pricing (AC 13).

### 7.2 NFR Exclusions

Explicit dispositions for domains not owned by this PRD (no silent omissions):

- **Tax computation NFRs**: Not applicable — owned by Tax Engine / Billing; rating-core MUST NOT compute tax.
- **Revenue recognition schedule performance**: Not applicable — Finance/Billing own recognition; this PRD supplies tagging/amounts only.
- **Spend-enforcement / real-time stop latency**: Not applicable — OSS / Policy Engine (real-time stop), Billing (post-aggregation cap), Finance (credit risk); Rating sets the amount, performs no enforcement.
- **Frontend UX performance / accessibility (WCAG) / i18n**: Not applicable to this backend PRD — owned by the corresponding frontend DESIGN.

## 8. Five Quality Vectors Analysis

| **Quality Vector** | **Show-Stopper Requirements** | **Rationale** |
|--------------------|-------------------------------|---------------|
| **Efficiency** | Evaluation MUST be cache-friendly (stored bindings, immutable prices by id, immutable snapshots) and call Pricing's `resolve` once per period and `ends_on`, never per usage event. | Usage pipelines are volume-heavy; CPU/IO waste raises unit cost of goods sold for cloud metering. |
| **Reliability** | Outcomes MUST be replay-deterministic; failures MUST be explicit (fail-closed), never best-guess pricing. | Financial correctness and partner trust require reproducible charges and defensible audits. |
| **Performance** | Hot-path and batch rating MUST scale horizontally per tenant/partition with bounded p95 latency under peak OSS usage (targets in §7.1). | Rating is on the monetization critical path; delays become revenue leakage or disputes. |
| **Security** | Tenant isolation for bindings, snapshots and contract overrides; delegation proofs for cross-tenant administration; immutable audit for changes. | Pricing data is commercially sensitive; cross-tenant leakage is a critical incident class. |
| **Versatility** | The model matrix (flat/per_unit/graduated/volume/package/hybrid; commitments when sourced) and derived usage types MUST extend without breaking snapshot contracts to Rating. | Channel business models evolve; rigid pricing cores force expensive parallel systems. |

## 9. Public Library Interfaces

> Rating is a backend pricing module (rating-core within the Rating domain), not a client library. Interfaces below are high-level contracts; concrete API schemas, endpoints, and DDL belong in DESIGN.

### 9.1 Public API Surface

#### Evaluation contract

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-interface-tariff-evaluation`

**Type**: conceptual evaluation contract (shape in Design)

**Stability**: stable (contract intent), schema unstable (Design owns)

**Description**: Given an evaluation context at `t`, returns a resolved price outcome (bound prices, model kind, tier thresholds, exact amounts), `pricingSnapshotRef`, discount lineage, and evaluation metadata (the binding of record, the usage rating policy, the derived usage declaration digest, the engine generation; applied coupons and `fxTableVersion` once those sources exist). Replay-safe and deterministic.

**Breaking Change Policy**: Major version bump for incompatible request/response changes; snapshot semantics are part of the contract.

### 9.2 External Integration Contracts

#### Rating handoff contract

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-contract-rating-handoff`

**Direction**: **bidirectional, intra-gear** (in-process since ADR-0002 / T-D-16 — not an external integration). Inbound to rating-core: the frozen evaluation context assembled by the pipeline — windowed `Q` from the [contract 13](DESIGN.md#contract-13) counters, the child window ([contract 14](DESIGN.md#contract-14)), the binding of record and the derived usage declaration. Outbound from rating-core: the resolved price outcome.

**Protocol/Format**: outbound — resolved price outcome + `pricingSnapshotRef` + the minimum-fee top-up at roll-up + dormant obligations (`TrueUpObligation`, `PeriodFloorCapObligation` only if Contracts defines them) + discount lineage; the pipeline ([contract 15](DESIGN.md#contract-15)) persists it and maps it to `RatedCharge` / `BillableItem`, and hands off to Billing ([contract 16](DESIGN.md#contract-16)).

**Compatibility**: Snapshot-referenced and replay-safe; the pipeline owns the Usage → RatedCharge path, dedup, and windowed `Q`, while rating-core stays pure and aggregates nothing.

#### Finance FX input contract

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-contract-finance-fx-input`

**Direction**: required from Finance

**Protocol/Format**: FX rate tables and lock policies with `fxTableVersion`; per-window rate-lock and invoice-period FX modes (Design).

**Compatibility**: Immutable frozen inputs; rating-core records `fxTableVersion` / locked-rate id; no implicit provider defaults.

#### Promotions coupon snapshot contract

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-contract-promotions-coupon`

**Direction**: required from Promotions

**Protocol/Format**: frozen coupon snapshot (`couponId`, `adjustmentType`, `value`, `settlementCurrency`, `applyPerTierBand`, `applyScope`, `stackSequence`, validity, applicability, redemption eligibility) (Design). On `main` promotions are a Pricing concern and deferred (pricing D-409); `resolve` returns no promotion and no source exists.

**Compatibility**: Fail-closed on missing `applyScope` / `stackSequence` under `ordered_stack`; Rating never infers coupon rules from mutable campaign UI state.

#### Billing periodState / obligation contract

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-contract-billing-periodstate`

**Direction**: bidirectional with Billing

**Protocol/Format**: Rating publishes one complete exact result per commercial fact revision (`BillableItemDeliveryV1`, pull `RatingRunReadV1::deliveries_since` / `find_runs`, events when a broker delivers) carrying full-precision lines with provenance, the minimum-fee top-up lines Rating computed (T-D-78) and any dormant obligations; Billing owns period state, aggregates per invoice line, rounds once (HALF_EVEN at the stored `currency_scale`, to a major-unit `PostedMoney`), computes a correction as the rounded corrected total minus the cumulative posted amount, and decides draft replacement vs credit/debit note (Design [contract 16](DESIGN.md#contract-16); Seam Atlas C07/C08). `BillingPeriodStateChanged` is an observational hint to Rating. No Billing gear exists yet (DECISIONS R-04/R-05).

**Compatibility**: rating-core MUST NOT round money (declared business rounding of quantities aside); Billing owns aggregation, rounding and the invoice line key (the binding carries the rounding policy, HALF_EVEN only on the SDK path; DECISIONS R-32).

#### Catalog / Pricing read-model input contract

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-contract-pricing-readmodel`

**Direction**: required from the Pricing gear (PriceBook model) and from Products (derived usage types)

**Protocol/Format**: Pricing's read contract `PricingReadV1` (ClientHub; REST `GET /bss-pricing/v1/resolve`, `GET /bss-pricing/v1/prices/{id}`; pricing D-419…D-425), built and frozen in pricing's golden consumer contracts (`gears/bss/pricing/pricing/tests/contract/*.json`):
- `resolve(ResolveQuery{catalog: {tenant_id}, revision_id, date, item_id?, pins: [PricePin{item_id, dimension_value?, price_id}]}) -> ResolvedBindings{plan_id, revision_id, cells: [ResolvedCell{selection, binding?}]}` — per item the whole chain matrix; a binding (`AcceptedBinding`: price, SKU version, unit, meter, usage rating policy, recurring period, `via_default`, invoice inputs) or uncovered; signup binds the price in force, renewal walks `all` and stops before `new` (D-420); refusals `PIN_FOREIGN`, `PIN_DUPLICATE`, `PINS_TOO_MANY`, `REVISION_NOT_PUBLISHED`, `REVISION_NOT_YET_AVAILABLE`, `REGISTRY_UNAVAILABLE`, `INCOMPLETE_COMMERCIAL_INPUTS`.
- `price(PriceQuery{catalog, price_id}) -> ImmutablePrice` — an approved price's money served forever, a cancelled one with its state (D-422, D-520); `current_revision(PlanQuery) -> RevisionRef`.
- Money is exact major-unit decimals per `PriceModel`; `currency_scale` and `rounding` come with the binding's `InvoiceInputs` (D-421, D-437).
- Products: a usage SKU's meter is a derived usage type `products.derived/<code>@<n>` (P-D-259) whose declaration Rating evaluates with `bss_products_sdk::derived` (P-D-229); a typed read of the stored declaration is **not** in products-sdk (UNKNOWN / EXTERNAL CONTRACT REQUIRED — DECISIONS R-31).
- Pricing's events (`prices_published`, `plan_revision_published`, `approval_unit_decided`, reference-lost events) are not consumed: Rating reads by pull.

**Compatibility**: Rating calls as the system subject `bss-rating.system` with pricing `plan:read` (resolve) and `price:read` (the pinned read) only (D-424); it resolves on the period start and at each binding's `ends_on` with the pins the fact carries, stores the binding of record verbatim and never resolves again for a correction (DECISIONS T-D-73, ADR-0003). *Superseded (2026-10-08)*: the pricing read-model contract over the canonical scope key, `priceEligibility` + `cohort`, the `prorationBasis` and `billingAnchorPolicy` enums, plan documents at a pinned catalog version, `PriceWindow*` events, bundle `sum_of_parts` components and publish-time validator registration.

#### Subscriptions input contract

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-contract-subscriptions-input`

**Direction**: required from Subscriptions

**Protocol/Format**:
- **Facts.** The period-fact stream: `BillableItemCreated(kind=recurring)` per `(subscriptionId, billing period, lineKey)`, money-free. Each fact carries:
  - the per-component traceability tuple;
  - the suspended interval(s) with the suspension-billing posture;
  - the period-start `payerTenantId`;
  - the `collectionPaused` marker.

  The stream is consumed as the **synthesis trigger for every period-driven unit** (T-D-33; Rating never derives a recurring period itself).
- **Required on each fact for the PriceBook adapter** (SUB-D-29; UPSTREAM_REQS §2.6):
  - the plan revision;
  - the period's **pins** (`PricePin`s; Subscriptions holds them per period — the first period from the accepted order's bindings, Orders D-162);
  - the committed quantity (`QuantityInterval`) for `per_unit`;
  - the billing period from Subscriptions' `BillingTerms` (cycle `Month`/`Year`, anchor `Calendar`/`SubscriptionStart`, UTC).
- **Plan change.** The plan-change `(changeEffectiveAt, changeMode)` policy.
- **One-time dedup.** One-time charges are deduplicated by Subscriptions per phase-entry occurrence (SUB-D-28).
- **Target** (Seam Atlas C04, proposed; DECISIONS R-03, R-20, R-25):
  - versioned commercial facts for recurring, usage and one-time lines, published at period opening;
  - attribution segments mapping resources to subscription lines;
  - sealed usage-scope proofs;
  - recovery reads by business key.

  Rating schedules its own child windows inside a fact; Subscriptions publishes no per-window timer.

**Compatibility**:
- Rating consumes the change mode and never decides it.
- Rating consumes the pins and never advances them: a renewal's new pins are a new fact version.
- Rating checks the fact's accepted price binding against the resolved binding (`binding_mismatch`).
- Rating is the only writer of its own snapshot (T-D-44); Subscriptions' documents still name the accepted binding `pricingSnapshotRef` (CONTRACT CONFLICT, DECISIONS R-29).
- *Superseded (2026-10-08)*: the active plan phase (`phase_id`), `priceEligibility` inputs (`activatedAt`, `cohort`) and the `quantitySource` seat count.

#### Contracts & Agreements input contract

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-contract-contracts-input`

**Direction**: bidirectional with Contracts & Agreements (a `p1` dependency; no Contracts implementation exists yet — DECISIONS R-11)

**Protocol/Format**: inbound — the ordered commitment-pool set (per-pool id, unit, `poolType ∈ {prepaid_drawdown, committed_rate}`, balance-as-of + `balanceVersion`, draw order, rollover, optional `overageRate`) frozen at context assembly; the period true-up clause (`commitmentBasis`, committed quantity/spend); negotiated RI-style reserved rates via a contract override. Contracts is a first-draft PRD; nothing is built (release-gated, §5.3). Outbound — one `CommitmentBalanceEffect` per pool-observing rated outcome (draw, refill, or zero-draw + overage marker — T-D-10/T-D-27), idempotent on the outcome's evaluation/correction key. Inbound triggers — T-D-10 re-resolution cascades for later-`balanceVersion` units and T-D-27 over-draw detections (Design: [11 §4.9](DESIGN.md#contract-11-4-9), [05 §4.1](DESIGN.md#contract-05-4-1)).

**Compatibility**: Contracts serializes per-pool `balanceVersion` and owns balances; Rating evaluates over frozen balances only (no live balance read) and never mutates one; the effect event schema is a cross-PRD obligation mirrored in the Contracts design.

## 10. Use Cases

#### Tariff and price-book editing

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-usecase-tariff-editor`

**Actor**: `cpt-cf-bss-rating-actor-product-manager`

> This use case is performed in **Pricing** (price books, entries, prices, plans and plan revisions) and **Products** (SKUs, derived usage types); Rating only consumes the result through `resolve`. It is kept here for traceability.

**Preconditions**:
- Published SKUs exist in Products; a price book exists in Pricing.

**Main Flow**:
1. Select the SKU and its entry in a price book.
2. Configure the entry's model (flat / per_unit / graduated / volume / package), its dimension key and, for usage, its usage rating policy.
3. Add dated prices and submit them, and the plan revision, for approval (Pricing's approval units).

**Postconditions**:
- Approved prices and a published plan revision are available to `resolve`.

**Alternative Flows**:
- **Plan or price check fails** (`METER_DUPLICATE`, `ITEM_UNCOVERED`, `TIER_TOP_CLOSED`, …): Pricing refuses the publish.

#### Partner price-overlay management

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-usecase-partner-priceoverlay`

**Actor**: `cpt-cf-bss-rating-actor-partner-admin`

> **Superseded (2026-10-08)**: Pricing has no price overlays. A partner's prices are a separate price book (or SKU) maintained through the tariff-editor flow above; mapping customer groups to books is a later Pricing decision.

**Preconditions**:
- A price book for the partner exists in Pricing.

**Main Flow**:
1. Select the partner's price book.
2. Maintain its entries and dated prices.
3. Submit the changes for approval.

**Postconditions**:
- The partner's plans bind prices from that book.

**Alternative Flows**:
- **A price would start in the past** (`WINDOW_START_IN_PAST`): Pricing refuses it.

#### Finance simulation of a future window

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-usecase-finance-simulation`

> **Deferred to Follow-on (T-D-32, 2026-08-01 — confirmed by the product owner 2026-08-01).** No design contract claims
> this use case, and evaluating a *candidate* (unapproved) price requires a draft read that Pricing's
> `resolve` does not serve (it binds approved prices only; [11 §4.2](DESIGN.md#contract-11-4-2)). A simulation surface is a named future
> increment with its own draft-read carve-out; the partner-overlay use case's "simulate
> against sample usage" step rides the same gate.

**Actor**: `cpt-cf-bss-rating-actor-finance-analyst`

**Preconditions**:
- A candidate price and a sample usage profile are available.

**Main Flow**:
1. Upload/select a sample usage profile.
2. Pick the candidate price.
3. Export the evaluation trace (bound prices, snapshot, ASC inputs).

**Postconditions**:
- Forecast and ASC inputs are explainable from a reproducible trace.

## 11. User Interaction and Design

| **Interface Name** | **Role** | **Steps** | **Mockup Screen** |
|--------------------|----------|-----------|-------------------|
| Tariff / price book editor (Pricing UI) | As a Product Manager, I define price books, entries, dated prices and plans so commercial behavior is explicit | 1. Select SKU and entry<br>2. Configure model and usage rating policy<br>3. Add dated prices and submit for approval | — |
| Partner price book (Pricing UI; overlays superseded) | As a Partner Admin, I maintain a partner's price book so channel economics are controlled | 1. Select the partner's book<br>2. Maintain dated prices<br>3. Submit for approval | — |
| Finance simulation (deferred) | As a Finance Analyst, I preview invoice impacts of a future price so forecasts and ASC inputs are explainable | 1. Upload/select sample usage profile<br>2. Pick candidate price<br>3. Export evaluation trace | — |

## 12. Acceptance Criteria

> **As a** platform operator **I want** deterministic, hierarchical price resolution **so that** usage-based revenue is reproducible, auditable, and compatible with Rating and Finance controls.

### Price resolution and determinism

**1. Single outcome per frozen context**
- **Given** a fixed evaluation context and a stored binding of record
- **When** two workers evaluate the same usage record
- **Then** they MUST produce identical resolved unit rates and pre-tax monetary amounts before the Billing tax stage
- **And** all divergences without input change MUST be treated as defects

**2. Hierarchy application order** — *Superseded (2026-10-08)*: Pricing has no price overlays and binds the price itself.
- **Given** a renewal whose pinned price 10 has an `all` successor 12 and then a `new` successor 15 (pricing golden `resolve_renewal_walk`)
- **When** Pricing resolves the renewal with the pin and Rating prices the period
- **Then** the binding MUST be 12 (a signup on the same date would bind 15), and Rating MUST price exactly that binding
- **And** the result MUST reference the binding of record (plan revision, pins, price id, money digest)

**3. Meter ambiguity rejection**
- **Given** a plan revision where two items claim one meter
- **When** Pricing publish checks run
- **Then** publish MUST be rejected (`METER_DUPLICATE`, fail-closed) before reaching production
- **Given** a contract override that introduces a metering dimension absent from the bound entry (once Contracts exists)
- **When** contract publish validation runs
- **Then** publish MUST be rejected (fail-closed)
- **Given** an invalid configuration that reached runtime (two charge lines of one plan revision and charge kind on the same `(skuId, dimensionKey)`, or an uncovered cell)
- **When** evaluation processes usage for that plan revision
- **Then** evaluation MUST fail-closed (`price_uncovered` / mapping error) and MUST NOT silently pick a default tier or price

### Pricing models

**4. Graduated vs volume semantics**
- **Given** a usage entry with model graduated and two or more tiers
- **When** `Q` spans multiple tiers
- **Then** charge MUST equal the marginal sum per the graduated rule
- **Given** the same numeric tiers on a volume entry
- **When** `Q` is in tier `k`
- **Then** charge MUST apply `P_k` to the entire `Q`
- **Given** a SKU with only one tier
- **When** evaluation runs as graduated or volume Variant A
- **Then** the monetary outcome MAY be identical; the configured model kind MUST still be persisted in metadata
- **Given** a `CalendarHour` rating window and cloudlet usage Q = 8 in 10:00–11:00 and Q = 12 in 11:00–12:00 on bands `[0, 10)` 0.02, `[10, ∞)` 0.015
- **When** the two hours are rated
- **Then** each hour MUST be rated alone — volume 0.16 + 0.18 = 0.34, never the combined-volume 0.30; graduated(12) = 0.23 (pricing PRD seam fixtures)

**5. Committed usage** (release-gated, R-11)
- **Given** a subscription with committed quantity `C_commit` and an overage rate for the period
- **When** measured usage `Q` exceeds `C_commit`
- **Then** evaluation MUST split usage into in-commit and overage portions
- **And** when the contract defines period-end true-up, MUST emit a `TrueUpObligation` (amount, period, contract reference) consumable by Billing — not an implicit posted charge

**5a. Per-unit (per-seat) pricing**
- **Given** a recurring `per_unit` entry with `unit_amount` and a fact whose committed quantity is `N` for the period
- **When** the period is rated
- **Then** the recurring charge MUST equal `unit_amount × N` (never metered usage `Q`), prorated by coverage, and the quantity MUST be recorded with the binding of record

**5b. Package (block) pricing**
- **Given** a `package` usage entry with `package_size` and `package_price`
- **When** `Q` in the rating window requires `ceil(Q / package_size)` blocks
- **Then** the charge MUST equal `ceil(Q / package_size) × package_price` (a partial block rounds up to one block); distinct from volume

**5c. Composite (derived) meter evaluation**
- **Given** a usage SKU that sells a derived usage type `products.derived/<code>@<n>` with `Sum` inputs (products P-D-229, P-D-259)
- **When** a rating window is evaluated
- **Then** Rating MUST fold each input per UTC hour, apply the formula per hour with `bss_products_sdk::derived::evaluate`, sum the hourly outputs into `Q`, and price `Q` by the entry's model
- **And** the declaration and its digest MUST be frozen in the binding of record and MUST NOT be authored by Rating

**5d. Level-based (gauge) aggregation — T-D-17 / T-D-39** (suspended, R-06)
- **Given** a derived usage type input with `GranuleFold = Peak` (or `TimeWeighted` with `max_hold_seconds`) and gauge samples for the window's hours (including one hour with a late-arriving higher sample and one hour with a sampling gap exceeding the hold)
- **When** the window `Q` is derived and the late sample then re-folds its hour
- **Then** `Q` MUST equal the **sum of the hourly outputs** (`Peak`: max sample per hour; `TimeWeighted`: step-integral with the level held at most `max_hold_seconds` — the gapped span reads **0** and an operator signal MUST raise, never a guessed level)
- **And** a late `Peak` sample MUST change only its own hour, while a late `TimeWeighted` sample MUST re-fold its own hour and the following hours whose held level it changes (§6.2, level-based aggregation), producing a `Q` change under the standard re-materialization (new `qVersion`, a new revision of that window); band and package math over this `Q` MUST be unchanged from the `Sum` case

### Time, versioning, currency

**6. Price coverage**
- **Given** a plan revision and a period start on which one item's chain has no price in force (`resolve` answers `uncovered`)
- **When** the period is rated
- **Then** evaluation MUST fail explicitly for billable usage (`price_uncovered`; no silent fallback, never a zero price)
- **And** a value's own price chain MUST be bound before the default chain, and `via_default` MUST be recorded when the default chain was used (pricing D-385, D-420)

**7. Mid-cycle price end**
- **Given** a temporary price whose `ends_on` falls inside a billing period, followed by its return price
- **When** usage and the recurring line span `ends_on`
- **Then** charges MUST be computed separately for the slice before `ends_on` (the temporary binding) and from `ends_on` (the binding `resolve` returns on that date for the same pins), each emitted without invoice rounding (full precision)
- **And** a successor price's start that the pin does not take (its `effective_to`) MUST NOT split the period (pricing D-425)
- **And** the invoice-period total MUST be computed by Billing as the sum of slice amounts followed by Billing's rounding — rating-core MUST NOT round
- **And** a prorated recurring component MUST use the covered UTC-seconds fraction (T-D-77)

**8. Multi-currency (rating-core FX abstraction)**
- **Given** price currency differs from invoice currency (once FX is released, R-07)
- **When** rating-core applies conversion
- **Then** the FX table version or locked rate id MUST be recorded in the evaluation result
- **And** conversion MUST NOT use implicit provider defaults without a policy record

### Retroactivity and corrections

**9. Posted period protection**
- **Given** an invoice already posted for period `P`
- **When** a retroactive price change is applied to usage in `P`
- **Then** the system MUST NOT alter posted invoice lines
- **And** Rating MUST publish a complete new result revision naming its predecessor, from which Billing derives the adjustment per immutability rules (T-D-50)
- **And** retroactive runs MUST separately record usage-observation time and pricing-policy decision time in the audit log

**10. Late-arriving usage into an aggregate window**
- **Given** a graduated, volume or package usage entry
- **When** usage arrives late into a window after some records were rated
- **Then** evaluation MUST deterministically re-resolve tier placement for the whole window-aggregated `Q` and publish a complete new result revision (no mutation of prior outputs)
- **And** if Billing has posted the period, Billing MUST apply posted-period protection to that revision (an adjustment it derives, never an edit of the posted invoice)
- **Design note**: Rating does not read `periodState`; the open/posted branch is applied by Billing to the new revision, and the re-pricing uses the window's binding of record without a new `resolve` call (T-D-42 refined by T-D-73, T-D-50)

### ASC 606 traceability

**11. ASC 606 traceable identifiers**
- **Given** an evaluation that produces a charge for a subscription
- **When** the result is emitted to Rating / Billing
- **Then** the resolved outcome MUST always include `performanceObligationRef` and `sspSnapshotPointer` (nullable when not applicable)
- **And** non-null values MUST be immutable once emitted — a later price or SKU change MUST NOT alter an emitted reference

### Rating window, renewal and minimum fee

**12. Rating window**
- **Given** a graduated or volume usage entry whose usage rating policy has `rating_window = BillingCycle`
- **When** usage records occur in two parts of the same billing period with quantities `Q1` and `Q2`
- **Then** tier counter `Q` MUST equal `Q1 + Q2` within that billing period, and MUST restart at the next period (`reset = rating_window_start`)
- **And** the usage rating policy (`policy_id`, `version`, `digest`) MUST be recorded in metadata and `pricingSnapshotRef`

**13. Tenant-scoped bindings** (*Superseded in part, 2026-10-08*: price overlays no longer exist)
- **Given** a binding read for seller tenant A
- **When** a caller of tenant B requests the run, its snapshot or its bindings
- **Then** the read MUST be refused as not found (DESIGN §4.8) and no price of tenant A MUST leak
- **Given** a contract/account override bound to a specific `payerTenantId` / `accountId` (once Contracts exists)
- **When** evaluation runs for a different payer/account
- **Then** that override MUST NOT apply and MUST NOT leak across tenants

**14. Minimum fee per price** (*Supersedes* the plan-phase and grandfathering criteria, which no longer have a subject)
- **Given** two dimension values rated 10 and 10 on one default-chain price with `minimum_fee` 30 for a full `BillingCycle` period
- **When** the parent result is rolled up
- **Then** the billed amount for that price MUST be 30 (one floor shared by values bound to one price), with a 10 `min_fee_topup` line
- **Given** two own-chain prices, each with `minimum_fee` 30, and the same usage
- **Then** each price carries its own floor and the billed amount MUST be 60 (pricing D-415)
- **Given** a `CalendarHour` or `resource`-scoped entry
- **Then** no minimum fee applies (Pricing refuses one; pricing D-503, D-504)

**15. Aggregate rounding**
- **Given** twelve fragmented 5-minute records for one continuous hour of the same derived usage input
- **When** the hour's quantity is computed
- **Then** the input MUST be folded once over the hour from the merged records — never per record — and any rounding MUST be the derived usage type's own `output_scale` / `output_round`, applied to the hour's output
- **And** the applied rounding MUST be recorded in metadata

### Promotions and coupons

**16. Coupon application order and stacking** (release-gated: promotions deferred, pricing D-409)
- **Given** a resolved line after the contract and commitment placeholders
- **When** two eligible coupons match the same line and stacking policy is `exclusive_best`
- **Then** exactly one coupon MUST apply — the one yielding the lowest charge
- **And** the result MUST record `couponId`, stacking policy, and pre-/post-discount amounts
- **Given** campaign-linked `ordered_stack` with sequence `[C1, C2]`
- **Then** C2 MUST apply to the amount produced after C1
- **Given** a graduated tier line total of 100 and a 10% coupon without `applyPerTierBand`
- **Then** the discount MUST be 10 on the line total, not per marginal band
- **Given** price currency EUR and billing currency USD with a price-currency coupon
- **Then** the coupon MUST apply before FX; billing-currency coupons MUST apply only after FX
- **Given** a hybrid plan whose candidate set holds a `usage`-scoped coupon and a `line_total`-scoped coupon under `exclusive_best`
- **Then** exclusivity MUST widen to one coupon per plan per period — the greater total benefit measured on each candidate's own scope — and MUST NOT let both apply as "different lines" (T-D-20)
- **And** at launch a `line_total`-scoped snapshot MUST instead fail closed (T-D-22 — no plan-scoped evaluation base), never applied to a single line nor downgraded to another scope

### Plan change and proration

**17. Plan-change proration within a period**
- **Given** a subscription changes from plan A to plan B at `changeEffectiveAt` inside one billing period
- **When** evaluation rates the period
- **Then** it MUST rate plan A over `[periodStart, changeEffectiveAt)` and plan B over `[changeEffectiveAt, periodEnd)` (half-open, UTC), each with its own plan revision, pins and binding of record
- **And** each slice MUST be emitted at full precision (Billing aggregates)
- **And** the recurring component MUST be prorated by the covered UTC-seconds fraction (T-D-77)
- **And** tier `Q` inside one rating window MUST continue across the change when the subscription, the priced SKU / meter / dimension and a compatible window identity match (T-D-36), and no price id change MUST reset it
- **And** corrections to an already-rated portion MUST be published as complete new result revisions via the Adjustment path (Billing derives the difference)
- **And** evaluation MUST consume `(changeEffectiveAt, changeMode)` and MUST NOT decide the change mode

### Cloud resource pricing

**18. Dimensional pricing**
- **Given** an entry with a dimension key (e.g. `region`) and dimension values present on the usage record
- **When** evaluation maps usage at `t` (step 2)
- **Then** each distinct `(skuId, dimension value)` MUST resolve to its own charge line and bound price, with no line collision
- **And** the bound dimension key and value MUST be recorded in the binding of record
- **Given** an entry with a dimension key but a record arrives with an empty or partial dimension value
- **Then** the record MUST NOT be silently priced as a single line; evaluation MUST price it on the default chain only where Pricing bound it there, or fail-closed — never guess

**19. Reservation pricing — consumption-flavor** (release-gated, R-11)
- **Given** a consumption-flavor `reservationMatch` covering part of the measured usage at `t`
- **When** evaluation runs the commitments slot of step 4
- **Then** the matched portion MUST be priced at the reserved rate and the remainder at the bound on-demand price
- **And** the reserved portion MUST be excluded from `commitmentPools[]` drawdown
- **And** the matched quantity MUST be excluded from the on-demand tier counter `Q` (the remainder re-bands from zero); the in-commit pool quantity is NOT excluded (pool-vs-reservation asymmetry)
- **And** the reservation-match identifier MUST be recorded in metadata and `pricingSnapshotRef`
- **Given** no `reservationMatch` is present (the case today)
- **Then** evaluation MUST price as pure usage

**20. Provisioned-capacity charging — capacity-flavor** (release-gated, R-11)
- **Given** a capacity-flavor `reservationMatch` with `reservedQuantity` (e.g. 100 GB disk) at `t`
- **When** evaluation runs the commitments slot of step 4 and measured usage is zero for the period
- **Then** evaluation MUST emit a `capacityCharge` = reserved rate x `reservedQuantity` (allocation billed regardless of usage)
- **And** the `capacityCharge` MUST NOT be reduced by absent usage and MUST NOT draw down `commitmentPools[]`
- **And** `reservedQuantity`, reserved rate, and flavor MUST be frozen in Rating's snapshot (`rsnap1`)

### Non-Functional Requirements (Show-Stoppers)

**1. Throughput and latency**
- **Given** peak usage ingestion rates per tenant partition
- **When** evaluation runs on the hot path or batch rating
- **Then** p95 latency MUST meet working-assumption targets (< 100 ms binding lookup, < 1 s overall rating path) and hot-path throughput MUST sustain >= 10M events/day/region
- **And** horizontal scaling MUST avoid cross-partition locks on the hot path

**2. Audit and segregation**
- **Given** a material price or plan-revision publish
- **When** the change is committed
- **Then** it MUST carry Pricing's approval-unit sign-off on the shared `bss-approval` engine (Rating runs no second workflow)
- **And** every Rating result MUST reference its binding of record, and every charge-affecting Rating operator action MUST emit an auditable event with actor, subject and request id

**3. Resilience**
- **Given** Pricing unavailable (`REGISTRY_UNAVAILABLE` / service unavailable) or an incomplete commercial input (`INCOMPLETE_COMMERCIAL_INPUTS`)
- **When** evaluation cannot obtain a consistent binding
- **Then** evaluation MUST fail safe (no partial pricing)
- **And** retries MUST be idempotent

## 13. Dependencies

| Dependency | Description | Criticality |
|------------|-------------|-------------|
| OSS / AMS (tenant identity & hierarchy) | `tenantId`, delegation proofs | `p1` |
| Pricing (PriceBook model) | Price books, entries, dated prices, plans and plan revisions; the selector of the bound price; `PricingReadV1::{resolve, price, current_revision}` built and frozen (pricing D-419…D-425). Rating's adapter is designed (DECISIONS T-D-73…T-D-78, ADR-0003) | `p1` |
| Products (SKU registry, derived usage types) | SKU versions; usage SKUs sell derived usage types (`products.derived/<code>@<n>`, P-D-259) that Rating evaluates with `bss_products_sdk::derived` (P-D-229). A typed read of a stored declaration is **missing** (DECISIONS R-31) | `p1` |
| OSS metering / Rating (usage dimension population) | Dimension values on each usage record; normalized usage quantity (values NOT produced here — recorded here) | `p1` |
| **Usage-collector usage feed (launch-gating)** | The usage-collector documents a replay-safe pull feed (`cpt-cf-usage-collector-fr-billing-usage-feed`; on upstream `main`: `read_usage_feed` with an opaque cursor, interval entries, invalidation entries — DESIGN §3.3, ADR-0011); its code does not implement it yet. Gates usage rating (UPSTREAM_REQS U-1, DECISIONS R-01) | `p1` |
| **Subscriptions pins on facts (launch-gating)** | Each commercial fact must carry the period's plan revision, pins, committed quantity and `BillingTerms` (SUB-D-29 decides the PriceBook adapter but defers it; Subscriptions has no code). Gates every rated line (DECISIONS R-03; UPSTREAM_REQS §2.6) | `p1` |
| Contracts & Agreements | Account-specific price terms, commitments, true-up clauses (first-draft PRD; release-gated) | `p1` |
| Subscriptions | Effective-dated Plan/Add-on links, subscription state, billing periods and `BillingTerms`, committed quantity, pins per period, `(changeEffectiveAt, changeMode)`; commercial facts; resource → subscription attribution (not specified by Subscriptions today — DECISIONS R-25) | `p1` |
| ~~Rating & Charging (downstream gear)~~ | **Not a dependency — intra-gear since ADR-0002 / T-D-16.** The Usage → RatedCharge pipeline, dedup, windowed `Q`, unit synthesis, rated-output persistence and Billing handoff are **this gear's own** design contracts 12–16 (authored 2026-07-15); the core↔pipeline contract is in-process, not an external integration. The upstream `PRD-rating-engine-202604031200` is legacy provenance (§2.2), not a live counterpart | — |
| Billing & Invoicing | Owns period state; consumes complete result revisions + snapshots; rounds invoice-line aggregates; posts immutable invoices and credit/debit notes to the ledger (`pricing_snapshot_ref` carries Rating's snapshot id). **No Billing gear exists** (DECISIONS R-04/R-05) | `p1` |
| IRM / usage emitters (completeness evidence) | Coverage declarations and historical resource inventory that prove a usage window complete (Seam Atlas C05, proposed). **IRM has no code** (DECISIONS R-21) | `p1` (usage invoicing) |
| Orders Lifecycle (consumer) | Calls pre-purchase evaluation at submit/amend with the accepted chain matrix (Orders D-154, D-167; DECISIONS T-D-79, R-24) | `p1` |
| Finance (FX) | FX rate tables and lock policies; `fxTableVersion`. **No pinnable FX source exists**; launch is native-currency only (DECISIONS R-07) | `p1` |
| Promotions / Discounts | Owned by Pricing and deferred (pricing D-409); no coupon source exists | `p2` |
| Spend control / credit risk | Billing (post-aggregation cap) + OSS/Policy (real-time stop) + Finance (credit risk / prepaid gating); Rating sets amount only, no enforcement | `p2` |
| BSS Architecture Manifest | §4.1 Catalog, §4.2 Rating, §4.4 Billing, §2.1.3 identities, §8 data model | `p1` |

## 14. Assumptions

- NFR targets are working assumptions (baselines from `PRD-metering-pricing-module-202601120119`) pending the program NFR workshop; capacity planning uses them until committed.
- rating-core is a pure, I/O-free crate within the one `rating` gear deployable (ADR-0002 / T-D-16), not a separate service; the earlier "logical module within the BSS Rating domain" manifest §4.2 note is superseded.
- The windowed `Q` is materialized and owned by the rating pipeline (Design [contract 13](DESIGN.md#contract-13)); rating-core receives `Q` as a frozen input.
- OSS metering will emit dimension values on usage; until then only an entry's default chain is usable and per-combination SKUs are the only workaround.
- Pricing supplies the GL code, tax category and invoice-line template with each binding (`InvoiceInputs`, pricing D-421) and Rating freezes them in the binding of record; Contracts/Finance supply SSP/PO and FX policy pointers once they exist; Rating consumes, never recomputes, supplied evidence.
- Promotions (a deferred Pricing concern, D-409) will provide a frozen coupon snapshot contract before production coupon rating; until then §17.2 is the Rating-side stub.
- Subscriptions will carry each period's pins, plan revision, quantity and billing period on its facts (SUB-D-29); until then nothing is rated end to end.

## 15. Open Questions

| **Question** | **Owner** | **Target Date** | **Answer** | **Date Answered** |
|--------------|-----------|-----------------|------------|-------------------|
| Numeric SLOs (p95/p99, max RPS per partition) for the pricing hot path | Program NFR workshop | TBD | Working assumption: p95 <= 100 ms per catalog lookup; p95 < 1 s overall rating path; >= 10M events/day/region. Final acceptance at NFR workshop. | — |
| Default anti-drift cap (`maxCumulativeMarkup`) value and clamp-vs-fail behavior across partner→reseller→customer | Program / Finance workshop | — | **Closed — superseded (2026-10-08)**: Pricing has no price overlays, so there is no chain to cap. | 2026-10-08 |
| Non-negative resolved price: clamp-to-zero vs emit-as-credit | Finance | TBD | §6.1 guard is normative; only the residual-handling policy is deferred. | — |
| Follow-on capabilities (percentage, min/cap per period, bilateral, two-dimensional) | Program workshop | TBD | Prioritize after Design lock for current Scope; see §17.4. Dimensional, CAPACITY/reservation, and composite meter are in Scope. | — |
| Promotions PRD field names and coupon snapshot event contract | Promotions + Design | TBD | Align with §17.2 before production coupon rating; Rating-side semantics are normative here. | — |
| **Plan-scoped coupon evaluation base** (dormant: promotions are deferred in Pricing, D-409) — which component holds a plan's line set for an `AnchorPeriod`, what triggers it (it cannot precede the last member line's rating, yet an open period keeps re-resolving usage), and how a member line's re-rate cascades into the plan-scoped discount and its split-back | Promotions + Rating Design | Before `line_total` coupons go live | **Open (T-D-22, 2026-07-28)**: the coupons slot of step 4 (former step 7) is per-line and every evaluation unit is sub-plan, so no such point exists today. Launch posture: `applyScope = line_total` **fails closed**, T-D-20's cross-scope exclusivity is inert, and per-line `exclusive_best` over `usage`/`recurring` is unaffected. | — |
| Formal confirmation of rating-core deployment model | Architecture / Program leadership | Before Design lock | **Superseded by ADR-0002 / T-D-16 (§14)**: rating-core is a pure crate inside the one `rating` gear deployable — the earlier "submodule of Rating vs standalone service" framing predates the consolidation (post-rename it read "submodule of itself"). Executive ack of ADR-0002 is the remaining formality. | — |
| Minimal cloud subset for a real S3 / VM / Disks catalog | PM Team | 2026-06-11 | Resolved: Dimensional in Scope; CAPACITY/reservation is release-gated (no source); **composite meters are in launch** — every usage SKU sells a Products derived usage type (≥ 1 input, P-D-229, P-D-259) and Rating evaluates it (T-D-39, T-D-76); VM MAY also be priced via a dimension value. | 2026-06-11 |
| Usage dimension-population contract (emission of `dimensionKey` values, field shapes, normalization) | OSS / Constructor Fabric Core (emission); Rating (declare/freeze) | TBD | BSS side closeable now (declare + freeze; Rating passes through). External dependency / critical path: the OSS metering emission shape. Until OSS emits values, `dimensionKey` stays empty. | — |
| (Finance) Launch without a hard spend cap / real-time spend stop — accepted? Owner of credit risk + prepaid gating | Finance | TBD | Rating owns no enforcement. Finance MUST accept launch without a ceiling, or name the gating owner (Billing post-aggregation cap / OSS-Policy real-time stop). | — |
| (Product + OSS/Policy) Free-tier level: per-meter $0 band vs per-account-per-service allowance; boundary behavior and enforcing domain | Product + OSS/Policy | TBD | Current Scope = a $0 band on the bound price of one `(skuId, dimension value)`; Pricing removed included quantities (D-467); a cross-account allowance is a new aggregate (Follow-on). | — |
| (Product + Finance) Per-resource minimum charge and stance on rapid create/delete churn | Product + Finance | TBD | `minimumCharge` MAY be configured per resource; churn policy undecided. | — |
| (Finance + Legal/Tax) "Discount vs tax" ordering per jurisdiction, and whether a contractual floor claws back coupon discount | Finance + Legal/Tax | TBD | Rating emits discount lineage for Billing/Tax; default proposal = floor compares post-coupon total. | — |
| (Operations / Portal) Owner of real-time consumption visibility + budget/limit alerts | Operations / Portal | TBD | Not a Rating requirement; name the Billing/Portal owner. | — |
| (Product + Metering + Products) Where are level meters (cloudlet peak-per-hour, storage GB-month) integrated? Products' derived usage types declare `Peak` / `TimeWeighted` input folds that Rating applies per hour; the usage-collector PRD requires emitters to pre-integrate levels into `SUM` meters and forbids charging from non-`SUM` folds | Product + Usage Collector + Products | Before level-billed products launch | Open (DECISIONS R-06). Until decided a non-`Sum` input fails closed (`unsupported_input_fold`). | — |
| (Program NFR workshop) The "p95 < 1 s overall rating path" target cannot hold end to end: a final result also waits for the finalization delay (minutes for hourly, ≥ 48 h for monthly profiles) and completeness evidence; restate it for the Rating-internal path | Program NFR workshop | Design lock | Open (DECISIONS R-13). | — |
| (Architecture + Pricing + Subscriptions + Billing) Reconcile the Seam Atlas v2 baseline (2026-09-30) with this repository: usage parent facts, attribution owner, one-time rating and finalization evidence; its Pricing model is now this repository's (PriceBook, T-D-37), the price-binding authority is decided by T-D-37, the floor executor by T-D-38, and Rating's pricing adapter by T-D-73…T-D-78 (R-17 closed) | Architecture with the named owners | Before Atlas contracts are implemented | Open (DECISIONS R-19…R-25). | — |
| (Pricing + Finance) Recurring proration basis: Rating proposes covered UTC seconds / period UTC seconds (T-D-77); Pricing's unbuilt slice-07 note says calendar days | Pricing + Finance | Before recurring rating launches | Open (DECISIONS R-30). | — |
| (Products) A typed read of a stored derived usage declaration for Rating (`DerivedUsageTypeReadV1`); today only REST `GET /bss-products/v1/derived-usage-types/{code}/versions/{n}` exists | Products | Before usage rating launches | Open (DECISIONS R-31). | — |
| (Subscriptions) Carry each period's plan revision, pins, committed quantity and `BillingTerms` on the commercial facts; who cuts a period at an `ends_on` (Orders records it as unagreed) | Subscriptions + Rating | Before any line is rated end to end | Open (SUB-D-29 adapter deferred; DECISIONS R-03). | — |
| (Finance) Launch without FX conversion — billing currency must equal the price row's currency (per-market rows cover multi-currency catalogs) | Finance | Before cross-currency billing is sold | Proposed (DECISIONS R-07); no pinnable FX source exists. | — |

## 16. Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| Usage dimension contract slips (OSS emission) | `dimensionKey` stays empty; per-combination meters explode catalog cardinality; S3/VM cannot be billed by dimension | Lock the BSS-side dimension contract now; raise OSS emission shape as an upstream Usage Collector requirement (critical path) — §17.3 |
| **Upstream contracts not implemented** (usage feed, Subscriptions pins on facts, the Products derived-declaration read, Billing) | Usage and period rating cannot run end to end; a polling workaround over the usage-collector's current query API would miss late-accepted records and invalidations | Track UPSTREAM_REQS §2 as launch-gating; build and verify `rating-core` against pricing's golden consumer contracts and the Atlas arithmetic vectors meanwhile (DESIGN §4.10) |
| **Seam Atlas v2 baseline diverges from this repository** (audited against another fork; its PriceBook Pricing model has since landed here, T-D-37, and Rating's pricing seam is re-cut against it, T-D-73) | Subscriptions' own documents still describe the pre-PriceBook model (only SUB-D-29 records the change), so a Subscriptions implementation could still carry `cohort` / `pricingSnapshotRef` instead of pins | Adopt only owner-consistent parts (DECISIONS T-D-48); track R-19…R-25 and the Subscriptions ask in UPSTREAM_REQS §2.6 |
| rating-core deployment reversed to standalone service | Manifest contradiction; integration rework | ADR-0002 / T-D-16 is the decided model (a pure crate inside the one `rating` gear); a reversal reopens the ADR, not this row — kept only to track the pending executive ack |
| Uncommitted NFR numbers (p95, throughput) | Blocks engineering capacity planning | Commit working-assumption NFRs at the program workshop before Design lock (§7.1, §15) |
| ~~Missing anti-drift cap on material multi-link chains~~ — **closed (2026-10-08)**: Pricing has no price overlays | — | Row kept as a historical record |
| ~~`PRD-rating-engine-202604031200` draft/empty~~ — **closed by ADR-0002 / T-D-16** | — | There is no external integration contract to define: the pipeline scope that PRD stubbed was absorbed into this gear and authored as design contracts 12–16 (2026-07-15). Row kept as a historical record |
| Coupon snapshot contract undefined (promotions deferred in Pricing, D-409) | Non-reproducible coupon rating | Treat §17.2 as the Rating-side stub; align field names/events before production coupon rating |

## 17. Reference Materials

| **Material** | **Link** | **Comments** |
|--------------|----------|--------------|
| Pricing PRD, DESIGN and DECISIONS (**pricing** gear — the PriceBook model) | `gears/bss/pricing/docs/` | Price books, entries, dated prices, plans and revisions; the read contract `resolve` / `prices/{id}` (D-419…D-425, `design/07-read-contract-events.md`); golden consumer contracts `pricing/tests/contract/*.json` |
| Products PRD and DECISIONS (**products** gear) | `gears/bss/products/docs/` | SKU registry and versions; derived usage types (P-D-229, P-D-259); `bss_products_sdk::derived` |
| Subscriptions PRD and DECISIONS (**subscriptions** gear) | `gears/bss/subscriptions/docs/` | Periods, committed quantity, `(changeEffectiveAt, changeMode)`; SUB-D-29 (pins per period, the PriceBook adapter), SUB-D-28 (one-time dedup) |
| Orders Lifecycle UPSTREAM_REQS and DECISIONS | `gears/bss/orders-lifecycle/docs/` | The order-evaluation contract Orders expects (D-154, D-167) and the accepted bindings at activation (D-162) |
| Usage Collector PRD — usage feed | `gears/system/usage-collector/docs/PRD.md` (`cpt-cf-usage-collector-fr-billing-usage-feed`) | The ingestion contract this gear's [contract 12](DESIGN.md#contract-12) consumes (launch-gating) |
| Trace chain | `AGENTS.md` (repository root) | Manifest → PRD → ADR → Design → Stories |
| BSS Architecture Manifest | `docs/bss/manifest/vz-arch-manifest-bss-only.md` | **Historical — not vendored into this repo**; §4.1 Catalog, §4.2 Rating, §2.1.3 identities |
| Project glossary | `docs/project-glossary.md` | **Historical — not vendored**; canonical terms |
| Metering & pricing predecessor | `docs/bss/prd/PRD-metering-pricing-module-202601120119/…` | **Historical — not vendored**; NFR baselines, pricing-hierarchy scope migrated here (§2.2) |
| Usage-based pricing platforms (benchmark) | Metronome, Lago, OpenMeter | Reference for cloud model coverage and scope sequencing (dimensional, composite, capacity/reservation) — §17.3 |

### 17.1 Evaluation Order (normative appendix)

Re-cut 2026-10-08 against the PriceBook model (DECISIONS T-D-73…T-D-78). For any child window evaluated at timestamp `t` (UTC) and context `ctx`:

1. **Bind**: take the binding of record — the bindings Pricing's `resolve` returned for the fact's plan revision and pins on the period start, and on each binding's `ends_on` inside the period (pricing D-397, D-420, D-425). Each `ends_on` starts a new slice; a successor's `effective_to` never does. An uncovered cell fails closed (`price_uncovered`); the fact's accepted binding must equal the resolved one (`binding_mismatch`). Rating does no selection: no scope key, phase, eligibility class, cohort or overlay. Corrections reuse the binding of record; only an administrative re-rate re-binds.
2. **Meter**: for usage, the window quantity `Q` over the binding's rating window (`BillingCycle` or `CalendarHour`, `reset = rating_window_start`, `fold = SUM`, `partial_window = actual_quantity_full_thresholds`; pricing D-514), keyed by the child's aggregation key and the policy's `aggregation_scope`. A usage SKU sells a Products derived usage type: each input is folded per UTC hour (`Sum` at launch; `Peak` / `TimeWeighted` suspended, R-06), the formula is applied per hour with `bss_products_sdk::derived::evaluate`, and `Q` is the sum of the hourly outputs (T-D-39, T-D-76). The aggregation is owned by the **rating pipeline** ([contract 13](DESIGN.md#contract-13)); **rating-core** receives `Q` frozen. Slices inside one window do not reset `Q`: graduated places marginally from the band offset of earlier slices; volume selects the band by the **window total**; package counts blocks once over the window. For recurring lines, the quantity is the fact's committed quantity, and each slice is prorated by its covered UTC-seconds fraction (T-D-77).
3. **Model formula**: the bound price's model and money (pricing D-386; `amount_for`): flat `amount`; per_unit `unit_amount × q`; graduated sum over bands; volume `q × rate` of the first band with `q < up_to`; package `ceil(q / package_size) × package_price`. Money stays exact in major units with the binding's `currency_scale` attached (T-D-74, T-D-80). Bands are half-open and the last band is open.
4. **Dormant placeholders** (fail closed if referenced; no source exists): contract/account override (Contracts, R-11), commitment pools and reservations (R-11), coupons (promotions deferred in Pricing, D-409), FX (R-07; a price book has one currency, so FX applies only when billing currency differs).
5. **Minimum-fee floor** (at parent roll-up): per price, per subscription, per billing period, before promotions: `max(Σ exact amounts of every slice and value rated by that price, minimum_fee × covered fraction)`, the difference a `min_fee_topup` line (pricing D-388, D-415; T-D-38, T-D-78). Only `BillingCycle` + `subscription_line` entries carry one.
6. **Guards and emit (rating-core → Billing boundary)**: no line is negative before the floor; amounts are exact reduced fractions in major units with `currency` and `currency_scale`; rating-core never rounds money — Billing rounds each invoice-line total once (HALF_EVEN at the stored `currency_scale`).

*Superseded (2026-10-08)*: the nine-step order — subscription composition and plan phase, base-row selection on the canonical scope key with eligibility classes and cohorts, meter mapping with `billingGranularity`, partner / OrgTier / brand / region overlays, the `PriceOverlay` scope mapping table and the anti-drift cap.

#### Determinism and Rating compatibility (preserved)

- **Pure function core**: determinism stated over the child window; for windowed models the window-aggregated `Q` for the child's aggregation key and rating window. Given frozen inputs, the monetary outcome MUST be identical across replay, recompute, and cross-region batch workers.
- **Windowed `Q` ownership**: materialized and owned by the rating pipeline (Design [contract 13](DESIGN.md#contract-13)); rating-core consumes it frozen; concurrent re-resolution of one unit serializes on that unit.
- **Non-negative resolved price**: MUST NOT go negative; clamp to zero or emit a structured credit (policy TBD).
- **Usage corrections / negative quantity**: deterministically reverse prior effect (refill pool, decrement `Q`) in a complete new result revision; Billing derives the compensating amount (T-D-50); never drive a line negative.
- **Snapshot carry / idempotency / correction idempotency / separation**: per §6.1.

#### Multi-currency (preserved)

- **Price currency**: the bound price's book currency (one per book); per-market prices are separate books and first-class.
- **Presentment currency**: portal display FX, non-authoritative, outside rating-core; MUST be labelled estimates.
- **Billing currency**: invoice currency per payer account/contract; rating-core converts at the FX placeholder (dormant, R-07); per-window rate-lock final at event time; invoice-period FX keeps a provisional estimate and finalizes with the close-time rate; a later correction is a new revision.
- **Coupons and currency**: price-currency coupons before FX; billing-currency coupons after FX (both dormant).
- **rating-core / Finance boundary**: FX tables and lock policies owned by Finance; rating-core records `fxTableVersion` / locked-rate id.
- **rating-core / Billing boundary**: rating-core MUST NOT apply invoice rounding; Billing rounds in billing currency after conversion.

### 17.2 Boundary Contracts (coupons, floor/cap, plan-change proration)

**Coupons (Promotions boundary; dormant — promotions deferred in Pricing, D-409)** — normative order extends §17.1: bound price → contract override → commitment → coupon → FX → emit. Coupons apply after the contract override and after commitment math; default before FX (price currency), exception after FX for `settlementCurrency = billing`. A partner's own price book is not a discount layer, so a coupon applies to the bound price whatever book it came from. Coupon + graduated tier: default on the total line amount after tier math; `applyPerTierBand = true` applies per marginal band. Stacking: `exclusive_best` (default — largest customer benefit, others excluded; per line, widening to **one coupon per plan per period** whenever a `line_total` candidate is present — T-D-20), `ordered_stack` (campaign-linked `stackSequence` only), incompatible pairs fail-closed at redemption bind. Consumption contract (Rating ← Promotions): a frozen coupon snapshot with at minimum `couponId`, `adjustmentType` (percent \| fixed_amount), `value`, `settlementCurrency` (price \| billing), `applyPerTierBand`, `applyScope` (`usage` \| `recurring` \| `line_total`), `stackSequence` (required under `ordered_stack`), validity, applicability filters, redemption eligibility. `line_total` is the **authoring-side** default Promotions writes — Rating never applies it as a fallback: a snapshot *arriving* without `applyScope` (or without `stackSequence` under `ordered_stack`) MUST fail-closed. **At launch `line_total` itself fails closed** (T-D-22 — no plan-scoped evaluation base; §6.8), so launch coupons MUST be authored with an explicit `usage` or `recurring` scope.

**Minimum fee and period-level floor/cap** — a price's `minimum_fee` is applied by Rating at parent roll-up, per price, per subscription, per billing period, before promotions, prorated by the covered fraction (pricing D-388, D-415; DECISIONS T-D-38, T-D-78; examples in AC 14). Pricing keeps no plan cap or plan minimum (D-388), so a period cap or plan-level floor exists only if Contracts defines one: Rating would then set its amount/currency/scope and emit a `PeriodFloorCapObligation`, and Billing would execute `max(total, floor)` / `min(total, cap)`. The non-negative guard applies before any floor; a floor MUST NOT mask a negative line. Whether a contractual minimum-spend floor claws back coupon discount is unresolved (default proposal: floor compares post-coupon total) — §15.

**Plan-change proration** — Subscriptions owns WHEN and the up/down asymmetry policy (cross-PRD); Rating owns the evaluation semantics and consumes `(changeEffectiveAt, changeMode)`. On a plan change at `changeEffectiveAt`, rate the old plan over `[periodStart, changeEffectiveAt)` and the new plan over `[changeEffectiveAt, periodEnd)` (half-open, UTC), each with its own plan revision, pins and binding of record, at full precision (Billing aggregates). Recurring component prorated by the covered UTC-seconds fraction (T-D-77). Tier `Q` inside one rating window continues across the change per the T-D-36 continuation key (subscription + priced SKU / meter / dimension + compatible window identity; no price id resets it); an incompatible usage rating policy is a new entry whose change Subscriptions schedules at the next UTC hour boundary (pricing D-510 E4); commitment pools would default to reset. Prorated corrections to an already-rated portion are published as complete new result revisions via the Adjustment path; Billing derives the difference. Rating consumes `changeMode` to pick the split point; the policy that sets the mode is Subscriptions.

### 17.3 Cloud Catalog Readiness and Phasing

The cloud-defining models for a genuine S3 + VM + Disks catalog: **Dimensional pricing** (one dimension key per entry, a price chain per value) and the **usage dimension-population contract** are in Scope; **CAPACITY / reservation pricing** is release-gated (no source). **Composite meters are in launch** — every usage SKU sells a Products derived usage type (P-D-229, P-D-259) and Rating evaluates the formula-as-data per hour (T-D-39, T-D-76); VM MAY also be priced via a dimension value. The engine seams (`dimensionKey`, `reservationMatch` + `capacityCharge`, `commitmentPools[]`) admit further models additively — no change to the published snapshot/Rating contract.

| **Item** | **Scope** | **Unlocks** | **Hard precondition (owner)** |
|----------|-----------|-------------|-------------------------------|
| Dimensional pricing | Scope | S3 by storage-class / region / operation; VM by instance type | OSS metering emission of dimension values (external; Pricing declares the key, Rating records and passes it through) — critical path |
| CAPACITY / reservation pricing | Release-gated | Provisioned Disks / IOPS, RI-style commitments | `reservationMatch` entitlement source and reserved rates (OSS / Contracts; Pricing defers reserved capacity) |
| Composite meter | **In launch** | VM = vCPU + RAM as one priced line | Derived usage type declared by Products (P-D-229), evaluated by Rating (T-D-39, T-D-76); a typed declaration read (R-31) |

**Sequencing**: (1) lock the BSS-side dimension contract (Pricing declares one key per entry; Rating records the bound value and passes it through) and raise the OSS metering emission shape as an upstream requirement to the Usage Collector PRD — the external emission is the critical path and blocks Dimensional pricing. (2) Dimensional → (3) CAPACITY/reservation once a source exists. Composite meters are in launch — Products stores the derived usage types (P-D-229, built) and Rating evaluates them (T-D-39); Rating needs a typed read of the declaration (R-31). **Risk if the dimension contract slips**: only an entry's default chain is usable and dimension combinations can only be expressed by minting a separate SKU per combination — exploding catalog cardinality.

### 17.4 Future Scope

**Tariff semantics — formulas and computation**

| **Capability** | **Priority** | **Status** | **Notes** |
|----------------|--------------|------------|-----------|
| Percentage pricing (% of base amount) | `p2` | Follow-on | Marketplace/payments; new model row in Design |
| Bounded override composition (anti-drift caps) | `p2` | Superseded | Pricing has no price overlays (2026-10-08) |
| Minimum fee (floor) per period | `p1` | In Scope | A price's `minimum_fee` is Rating's at roll-up (T-D-38, T-D-78); other period-level floors only if Contracts defines them (§17.2) |
| Cap (ceiling) per period | `p2` | Follow-on | Boundary/contract defined now (§17.2); bill-shock protection executed by Billing post-aggregation; impl phased |
| Two-dimensional pricing (seats x usage) | `p2` | Follow-on | Multiple meters + hybrid model; Subscriptions seat count input |
| Meter aggregation functions `last` / `unique` | `p2` | Follow-on | Products' derived usage types declare `Sum`, `Peak` and `TimeWeighted` input folds (P-D-229); `Peak` / `TimeWeighted` are **suspended** in Rating (DECISIONS R-06), and `last` / `unique` exist nowhere |
| Non-negative price after stacked discounts | `p3` | Deferred | Guard is normative (§6.1); only the clamp-vs-credit policy is deferred to Finance workshop |

**Plan structure and effective dating**

| **Capability** | **Priority** | **Status** | **Notes** |
|----------------|--------------|------------|-----------|
| Extended multi-SLA tier packs | `p2` | Follow-on | `PlanTier` is out of scope in Products; tier packs would be separate SKUs or plans |
| Plan change policy (immediate vs end-of-term, asymmetric up/down) | `p2` | Cross-PRD | WHEN/asymmetry owned by Subscriptions; Rating proration semantics defined now (§17.2, AC 17) |

**Commitments and reservations**

| **Capability** | **Priority** | **Status** | **Notes** |
|----------------|--------------|------------|-----------|
| Commitment rollover (burn vs carry) | `p2` | Follow-on | Per-pool policy on `commitmentPools[]` (the commitments slot of step 4); additive |
| Multi-pool waterfall drawdown | `p2` | Follow-on | Enterprise contracts; additive over the ordered `commitmentPools[]` waterfall |
| Free tier as structural concept | `p2` | Follow-on | Current Scope expresses free only as a $0 band on a bound price; Pricing removed included quantities (D-467); a cross-account allowance is a new aggregate |
| Multi-year ramp, convertible RI, sustained-use auto-discount | `p3` | Deferred | Enterprise/cloud advanced |

**Cloud-specific models**

| **Capability** | **Priority** | **Status** | **Notes** |
|----------------|--------------|------------|-----------|
| Bilateral pricing (source x destination) | `p2` | Follow-on | An entry has at most one dimension key (pricing D-385), so `(source, destination)` would be one combined value or a pricing change |
| BYOL / license-attached discount | `p2` | Cross-PRD | Entitlement in OSS/Contracts; Rating consumes license flag in ctx |
| Retroactive volume tier on monthly accumulation | `p2` | Follow-on | Batch re-rate at period close; open-window late-arrival semantics defined now (AC 10) |
| Burstable credits, storage tier transitions, spot pricing | `p3` | Deferred | Cloud provider advanced |

---

*Child artifacts: [ADR-0003](./ADR/0003-cpt-cf-bss-rating-adr-pricebook-bindings.md) (rate the bindings Pricing resolves); DESIGN for the rating-core ↔ pipeline, Pricing, Products, Subscriptions and Billing integration contracts and evaluation traces.*
