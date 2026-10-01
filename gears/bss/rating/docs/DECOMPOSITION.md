# Decomposition: Rating

**Overall implementation status:**
- [ ] `p1` - **ID**: `cpt-cf-bss-rating-status-overall`

<!-- toc -->

- [1. Overview](#1-overview)
- [2. Entries](#2-entries)
  - [2.1 Rating Foundation - HIGH](#21-rating-foundation---high)
  - [2.2 Deterministic Evaluation Core - HIGH](#22-deterministic-evaluation-core---high)
  - [2.3 Price Adjustments: Overlays, Commitments, Coupons and FX - HIGH](#23-price-adjustments-overlays-commitments-coupons-and-fx---high)
  - [2.4 Usage Capture and Normalization - HIGH](#24-usage-capture-and-normalization---high)
  - [2.5 Attribution and Window Counters - HIGH](#25-attribution-and-window-counters---high)
  - [2.6 Commercial Facts and Window Scheduling - HIGH](#26-commercial-facts-and-window-scheduling---high)
  - [2.7 Child Evaluation and Finalization - HIGH](#27-child-evaluation-and-finalization---high)
  - [2.8 Corrections, Replay and Administrative Re-rate - HIGH](#28-corrections-replay-and-administrative-re-rate---high)
  - [2.9 Parent Roll-up and Billing Delivery - HIGH](#29-parent-roll-up-and-billing-delivery---high)
  - [2.10 Pre-Purchase Order Evaluation - MEDIUM](#210-pre-purchase-order-evaluation---medium)
  - [2.11 Operations, Reconciliation and Data Lifecycle - HIGH](#211-operations-reconciliation-and-data-lifecycle---high)
- [3. Feature Dependencies](#3-feature-dependencies)
  - [3.1 Upstream and release prerequisites](#31-upstream-and-release-prerequisites)
- [4. Contract address index](#4-contract-address-index)
  - [4.1 Identifier migration](#41-identifier-migration)

<!-- /toc -->

## 1. Overview

Rating is decomposed into one shared foundation and ten capability features. The boundaries follow
the runtime components and their transaction boundaries in [DESIGN.md](DESIGN.md) §3.2, not the
sixteen contract namespaces of [DESIGN §6](DESIGN.md#6-detailed-architecture-contracts):

- **The pure core is one crate and two features.** `rating-core` is I/O-free (ADR-0002). The step
  order re-cut against the PriceBook model (T-D-73: bind the resolved price, evaluate the derived
  meter, apply the model formula, floor at roll-up, guards) with exact arithmetic, engine
  generations, split points and proration is needed for the first rated line, and nothing external
  blocks the core. The price-adjustment steps (contract overlays, commitments, coupons, FX) have no
  source on `main` — Pricing removed `PriceOverlay`, Promotions are deferred (pricing D-409) — so
  they run with empty inputs and fail closed when referenced (R-07, R-11); they are a separate
  review and test boundary with their own launch gating.
- **The pipeline splits where its transactions split.** Usage capture is one page transaction per
  feed source; attribution and counters are fed by that page and by the inbox; fact intake, the
  scheduler and the wake scanner create and wake children; the rater evaluates one child per
  transaction; roll-up and delivery work one fact per transaction; a correction or re-rate is an input
  change that the same rater and roll-up absorb, plus a durable run.
- **Shared infrastructure is built once.** Persistence, the `bss_rating` namespace, tenancy and the
  operation matrix, the inbox, exceptions, work queues, coordination and the SDK contract rules are
  used by every pipeline feature, so they form the foundation.
- **Consumer surfaces are separate when their consumer or priority differs.** Order evaluation has
  its own consumer (Orders Lifecycle), SDK trait and open priority (R-24). Operations — the operator
  REST plane, reconciliation, data lifecycle and NFR verification — need every pipeline feature
  first.

Each feature is an implementation and test boundary inside the one `rating` gear. Features are not
independently deployable services.

[DESIGN.md](DESIGN.md) (`cpt-cf-bss-rating-design-main`) owns architecture, schemas, interfaces,
evaluation semantics and their rationale, including the detailed contracts of each namespace in §6.
This document owns feature scope, requirement allocation and build order. The `features/` documents
own flows, processes, states, definitions of done and acceptance criteria, and keep the runtime
procedures of their namespaces in their §7. [UPSTREAM_REQS.md](UPSTREAM_REQS.md) owns what Rating
needs from other gears; [DECISIONS.md](DECISIONS.md) owns decisions. Existing design IDs are
unchanged except where the kit's ID kinds required a new kind (§4.1).

All implementation checkboxes are unchecked: Rating has no code yet (`gears/bss/rating` holds
`gear.toml` and `docs/` only), and an authored specification is not evidence of running code. Open
decisions in DECISIONS and asks in UPSTREAM_REQS stay open; this decomposition resolves none of them.

**Coverage policy.** Each of the PRD's 44 functional and four non-functional requirements has exactly
one primary feature below; the PRD's interface and contract IDs are allocated the same way. A
feature's own "Requirements" line names its primary requirements first and its supporting ones
after them. Supporting features inherit the shared invariants and take part in integration tests.
Each entry lists the gear-wide principles and constraints it carries obligations for; the principles,
constraints, technology choices and entities of a contract namespace are covered by the entry that
lists that namespace under "Detailed contract coverage". An ID may appear in several entries: that
means a distinct feature-local obligation, never a second definition. A sequence or component that
spans features (`seq-ingest-usage`, `seq-correction`, `component-consumer-contracts`,
`component-billing-handoff`) is listed in each entry with the part it owns. The
`cpt-cf-bss-rating-dbtable-rerate-run` block of DESIGN §3.7 documents several small tables; each is
created by the feature that uses it: `bss_rating__operation` by foundation, `bss_rating__rerate_run` and
`bss_rating__rerate_target` by corrections and re-rate, `bss_rating__billing_hint` by roll-up and
delivery, `bss_rating__retention_hold` by operations, `bss_rating__balance_effect` (dormant) by child
evaluation.

## 2. Entries

### 2.1 [Rating Foundation](features/01-foundation.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-feature-foundation`

- **Purpose**: Every pipeline path is "at-least-once delivery + one local transaction + a
  deterministic key" (DESIGN §4.2), runs under one tenancy and authorization model (§4.8), and talks
  to other gears only through ClientHub SDKs and one inbox. Building these once, before any
  capability, makes idempotency, tenant isolation and fail-closed intake assertable in one place.

- **Depends On**: None (feature-level). Platform prerequisites are in §3.1.

- **Scope**:
  - The `cf-gears-bss-rating` module with the `stateful` lifecycle, `db_namespace = "bss_rating"`
    (T-D-60), migrations and the `bss_rating__outbox` queues `rating.child_work`, `rating.rollup`,
    `rating.delivery`.
  - Tenancy: `tenant_id` ownership columns, `Scopable` entities, `SecureConn` repositories, the GTS
    resource types and the operation matrix of DESIGN §4.8 (T-D-70), audited operator actions.
  - The inbox for every non-usage upstream fact (T-D-56) with quarantine and release; the exception
    register; the operation idempotency store.
  - The `LeaseProvider` port over the Cluster SDK (R-27) and the event-consumer scaffolding, disabled
    by default (T-D-71).
  - `cf-gears-bss-rating-sdk` V1 compatibility rules and golden DTO fixtures; ClientHub-registered
    provider test doubles behind `test-providers`.
- **Out of scope**:
  - Any rating semantics, usage interpretation, scheduling or result construction. The foundation
    knows no price and no window.
  - Defining another gear's events or SDK; Rating requests them (UPSTREAM_REQS).

- **Requirements Covered**:

  - None as primary owner. The foundation supplies shared mechanisms for `cpt-cf-bss-rating-fr-idempotency` (inbox and operation keys; primary: §2.8), `cpt-cf-bss-rating-nfr-resilience` (idempotent transactions, inbox; primary: §2.7) and `cpt-cf-bss-rating-nfr-audit-segregation` (audited operator actions; primary: §2.2).

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-bss-rating-principle-fail-closed`
  - [ ] `p1` - `cpt-cf-bss-rating-principle-single-writer-cc`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-bss-rating-constraint-domain-boundaries`
  - [ ] `p1` - `cpt-cf-bss-rating-constraint-in-process-contracts`
  - [ ] `p1` - `cpt-cf-bss-rating-constraint-inprocess-boundary-cc`
  - [ ] `p1` - `cpt-cf-bss-rating-constraint-no-extension-cc`

- **Domain Model Entities**:
  - Inbox entry, Exception (DESIGN §3.1 Rating-owned objects)
  - [ ] `p1` - `cpt-cf-bss-rating-entity-model-cc`

- **Design Components**:

  - [ ] `p1` - `cpt-cf-bss-rating-component-consumer-contracts`
  - [ ] `p1` - `cpt-cf-bss-rating-tech-stack-main`
  - [ ] `p3` - `cpt-cf-bss-rating-topology-main`

- **API**:
  - No capability endpoint. Shared: SDK error mapping to `CanonicalError`, RFC 9457 problem details,
    keyset cursors and the V1 compatibility rules of DESIGN §3.3.
  - Event intake scaffolding: `cpt-cf-bss-rating-interface-events` (consumed events, disabled).

- **Sequences**:

  - None separately identified in DESIGN §3.6; the inbox step of every flow uses this feature.

- **Data**:

  - [ ] `p1` - `cpt-cf-bss-rating-dbtable-inbox`
  - [ ] `p1` - `cpt-cf-bss-rating-dbtable-exception`
  - `bss_rating__operation` (part of `cpt-cf-bss-rating-dbtable-rerate-run`), the outbox tables.

- **Detailed contract coverage**: [11 §4.11](DESIGN.md#contract-11-4-11) inbox and recovery,
  [11 §4.13](DESIGN.md#contract-11-4-13) contract governance, [11 §4.12](features/01-foundation.md#contract-11-4-12)
  provider test doubles, [10 §4.6](DESIGN.md#contract-10-4-6) authorization catalog, [11 §1.3](DESIGN.md#contract-11-1-3),
  [11 §3.8](DESIGN.md#contract-11-3-8).
  - **Interfaces**: `cpt-cf-bss-rating-interface-context-inputs-cc`
  - **Data**: `cpt-cf-bss-rating-db-none-cc`
  - **Deployment**: `cpt-cf-bss-rating-topology-cc`, `cpt-cf-bss-rating-tech-stack-cc`

- **Phase**: 0.

---

### 2.2 [Deterministic Evaluation Core](features/02-evaluation-core.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-feature-evaluation-core`

- **Purpose**: Every rated amount is `rating-core`'s output for a frozen input, under a named engine
  generation, byte-identical on any worker at any later time. Isolating the pure core lets it be built
  and proven against Rating's own golden vectors before any upstream contract exists.

- **Depends On**: None (feature-level). Arithmetic is checked by Rating's golden vectors against pricing's `amount_for` oracle and pricing's golden contract and seam fixtures; the joint `bss-fixtures` corpus was deleted.

- **Scope**:
  - `EvaluationInput` / `EvaluationOutcome`, the compiled step order (PRD §17.1 steps 1–6), input
    validation, emission guards and the closed error taxonomy.
  - Bind, meter, model: the line's `AcceptedBinding` from `PricingInput` and the cross-check against
    the fact's accepted binding (`binding_mismatch`, `price_uncovered`; T-D-73), money from `PriceModel` decimals
    kept exact in major units with `currency` and `currency_scale` (T-D-74, T-D-80), derived usage meters per granule through `bss_products_sdk::derived` (T-D-76), the
    model formulas, granularity, window geometry from `UsageRatingPolicy` (T-D-75), split points at
    `ends_on` and band continuity across slices.
  - Guards and period math: proration by covered UTC seconds (T-D-77), plan-change splits, and the
    minimum-fee floor function used at roll-up (T-D-78).
  - `ExactAmount` (major-unit fraction with `currency` and `currency_scale`) / `ExactQuantity` arithmetic with checked budgets, canonical text and the bounds of DESIGN §3.3 (T-D-80); engine generations,
    `EngineRegistry`, `engine_digest`; golden vectors per retained generation.
  - Governance pass-through: invoice inputs from the binding, null ASC 606 references, the remaining
    defensive rating-time check (`rating-val-02`, `non_injective_mapping`); bundles are dormant (pricing D-411).
  - `evaluate_order` (the pure function under order evaluation).
- **Superseded requirements (T-D-73)**: `cpt-cf-bss-rating-fr-base-catalog-selection`,
  `cpt-cf-bss-rating-fr-price-eligibility-grandfathering` and `cpt-cf-bss-rating-fr-plan-phases`
  stay allocated here so they stay traced, but they are **superseded**: Pricing's `resolve` binds the
  price (signup: the price in force; renewal: walk `all`, stop before `new`, pricing D-420), and
  PriceBook has no cohort, eligibility class or phase (pricing D-386, D-397; trials dropped). This
  feature delivers them only as the binding cross-check. `cpt-cf-bss-rating-fr-period-floor-cap-obligation`
  is delivered as the minimum-fee floor (T-D-78); plan-level floors and caps no longer exist
  (pricing D-467). `cpt-cf-bss-rating-fr-publish-approval-governance` is delivered as rating-time
  checks only: approval is the shared `bss-approval` engine and Rating has no role (R-12 closed).
- **Out of scope**:
  - The dormant step-4 slots (feature 03), any persistence or I/O, aggregation of usage, the pipeline's input
    assembly and the pricing adapter (feature 07).
  - Publish-time enforcement inside pricing (`…-upreq-pricing-validator-hook`, closed: no hook exists or is needed, R-12).

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-bss-rating-fr-deterministic-evaluation-api`
  - [ ] `p1` - `cpt-cf-bss-rating-fr-single-outcome-determinism`
  - [ ] `p1` - `cpt-cf-bss-rating-fr-non-negative-price`
  - [ ] `p1` - `cpt-cf-bss-rating-fr-evaluation-order`
  - [ ] `p1` - `cpt-cf-bss-rating-fr-base-catalog-selection`
  - [ ] `p1` - `cpt-cf-bss-rating-fr-price-eligibility-grandfathering`
  - [ ] `p1` - `cpt-cf-bss-rating-fr-plan-phases`
  - [ ] `p1` - `cpt-cf-bss-rating-fr-flat-pricing`
  - [ ] `p1` - `cpt-cf-bss-rating-fr-per-unit-pricing`
  - [ ] `p1` - `cpt-cf-bss-rating-fr-tiered-graduated`
  - [ ] `p1` - `cpt-cf-bss-rating-fr-volume-variant-a`
  - [ ] `p2` - `cpt-cf-bss-rating-fr-package-pricing`
  - [ ] `p1` - `cpt-cf-bss-rating-fr-hybrid-pricing`
  - [ ] `p1` - `cpt-cf-bss-rating-fr-level-aggregation`
  - [ ] `p1` - `cpt-cf-bss-rating-fr-meter-mapping-granularity`
  - [ ] `p1` - `cpt-cf-bss-rating-fr-billing-granularity`
  - [ ] `p1` - `cpt-cf-bss-rating-fr-dimensional-pricing`
  - [ ] `p1` - `cpt-cf-bss-rating-fr-composite-meter-eval`
  - [ ] `p2` - `cpt-cf-bss-rating-fr-period-floor-cap-obligation`
  - [ ] `p2` - `cpt-cf-bss-rating-fr-mid-cycle-proration`
  - [ ] `p2` - `cpt-cf-bss-rating-fr-plan-change-proration`
  - [ ] `p2` - `cpt-cf-bss-rating-fr-asc606-traceable-identifiers`
  - [ ] `p1` - `cpt-cf-bss-rating-fr-publish-approval-governance`
  - [ ] `p1` - `cpt-cf-bss-rating-nfr-audit-segregation`
  - [ ] `p1` - `cpt-cf-bss-rating-interface-tariff-evaluation`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-bss-rating-principle-pure-function-core`
  - [ ] `p1` - `cpt-cf-bss-rating-principle-fixed-rule-order`
  - [ ] `p1` - `cpt-cf-bss-rating-principle-adopt-the-sor`
  - [ ] `p1` - `cpt-cf-bss-rating-principle-fail-closed`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-bss-rating-constraint-stateless-hot-path`
  - [ ] `p1` - `cpt-cf-bss-rating-constraint-exact-no-rounding`

- **Domain Model Entities**:
  - `EvaluationInput`, `EvaluationOutcome`, `RatedLine`, `ExactAmount`, `ExactQuantity`, `EngineId`
  - [ ] `p1` - `cpt-cf-bss-rating-entity-model-fnd`
  - [ ] `p1` - `cpt-cf-bss-rating-entity-model-sel`
  - [ ] `p1` - `cpt-cf-bss-rating-entity-model-mm`
  - [ ] `p1` - `cpt-cf-bss-rating-entity-model-ppc`
  - [ ] `p1` - `cpt-cf-bss-rating-entity-model-gov`

- **Design Components**:

  - [ ] `p1` - `cpt-cf-bss-rating-component-foundation`
  - [ ] `p1` - `cpt-cf-bss-rating-component-selection-eligibility`
  - [ ] `p1` - `cpt-cf-bss-rating-component-metering-models`
  - [ ] `p2` - `cpt-cf-bss-rating-component-period-plan-change`
  - [ ] `p2` - `cpt-cf-bss-rating-component-governance-asc606`
  - [ ] `p1` - `cpt-cf-bss-rating-component-evaluation-core-fnd`
  - [ ] `p1` - `cpt-cf-bss-rating-component-selection-evaluator-sel`
  - [ ] `p1` - `cpt-cf-bss-rating-component-metering-models-mm`
  - [ ] `p1` - `cpt-cf-bss-rating-component-period-plan-change-ppc`
  - [ ] `p1` - `cpt-cf-bss-rating-component-governance-gov`

- **API**:
  - In-process `rating-core` API: `cpt-cf-bss-rating-interface-core-evaluate` (`EngineRegistry`,
    `evaluate`, `evaluate_order`, `split_points`, `window_geometry`). No REST, no SDK trait.

- **Sequences**:

  - None separately identified in DESIGN §3.6; the core is called from `cpt-cf-bss-rating-seq-evaluate-tariff` (feature 07).

- **Data**:

  - None: `rating-core` owns no store (`cpt-cf-bss-rating-constraint-stateless-hot-path`). Namespace data statements: `cpt-cf-bss-rating-db-none-fnd`, `cpt-cf-bss-rating-db-none-sel`, `cpt-cf-bss-rating-db-none-mm`, `cpt-cf-bss-rating-db-none-ppc`, `cpt-cf-bss-rating-db-none-gov`.

- **Detailed contract coverage**: namespaces [01](DESIGN.md#contract-01), [02](DESIGN.md#contract-02),
  [03](DESIGN.md#contract-03), [09](DESIGN.md#contract-09), [10](DESIGN.md#contract-10) (except 10 §4.6);
  their §3.6 procedures are in [the feature's §7](features/02-evaluation-core.md#7-detailed-behavior-contracts).
  - **Interfaces**: `cpt-cf-bss-rating-interface-evaluate-fnd`, `cpt-cf-bss-rating-interface-reresolve-fnd`, `cpt-cf-bss-rating-interface-select-base-row-sel`, `cpt-cf-bss-rating-interface-price-line-mm`, `cpt-cf-bss-rating-interface-period-obligation-ppc`, `cpt-cf-bss-rating-interface-split-evaluation-ppc`, `cpt-cf-bss-rating-interface-validator-registration-gov`, `cpt-cf-bss-rating-interface-asc606-envelope-gov`
  - **Deployment**: `cpt-cf-bss-rating-topology-fnd`, `cpt-cf-bss-rating-topology-sel`, `cpt-cf-bss-rating-topology-mm`, `cpt-cf-bss-rating-topology-ppc`, `cpt-cf-bss-rating-topology-gov`, `cpt-cf-bss-rating-tech-stack-fnd`, `cpt-cf-bss-rating-tech-stack-sel`, `cpt-cf-bss-rating-tech-stack-mm`, `cpt-cf-bss-rating-tech-stack-ppc`, `cpt-cf-bss-rating-tech-stack-gov`

- **Phase**: 0 — build step 1 in §3: no dependency blocks it (the bindings come from the pricing fake over pricing's golden contracts until fact intake binds live).

---

### 2.3 [Price Adjustments: Overlays, Commitments, Coupons and FX](features/03-price-adjustments.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-feature-price-adjustments`

- **Purpose**: The price-adjustment steps would change a model amount by contract terms,
  commitments and reservations, coupons and currency conversion. None has a source on `main`:
  `PriceOverlay` is removed from Pricing (spec item 2), Promotions are Pricing's and deferred
  (D-409), reserved capacity is "later", Contracts has a draft PRD only, and a book has one currency
  (R-07, R-11). Their launch behaviour is to evaluate with empty inputs and fail closed when a
  reference demands a missing source; keeping them one boundary makes that posture testable and lets
  each step activate, as a new engine generation, when its source arrives.

- **Depends On**: `cpt-cf-bss-rating-feature-evaluation-core`

- **Scope**:
  - Price overlays (the former step 4): superseded — no input exists upstream or can be named.
    Slot 4a contract override and the anti-drift composition cap (`maxCumulativeMarkup`;
    `composition_cap_missing`, `contract_dimension_undeclared`), dormant until a Contracts source exists.
  - Slot 4b reservations (consumption flavor, capacity charge, reserved-rate two-source rule) and
    commitment pools (waterfall, true-up obligations) — pools dormant (R-11).
  - Slot 4c coupons (placement, stacking policies, `applyScope`, frozen snapshot) — no source (R-11).
  - Slot 4d currency roles and FX policies — native currency only at launch (R-07).
- **Out of scope**:
  - Owning or mutating any balance, redemption or rate; publishing `CommitmentBalanceEffect`
    (feature 07, dormant).
  - Contracts, Promotions and FX contracts themselves (UPSTREAM_REQS §2.9, §2.10).

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-bss-rating-fr-priceoverlay-scope-mapping`
  - [ ] `p1` - `cpt-cf-bss-rating-fr-overlay-stacking`
  - [ ] `p1` - `cpt-cf-bss-rating-fr-customer-contract-overlay`
  - [ ] `p2` - `cpt-cf-bss-rating-fr-bounded-composition-cap`
  - [ ] `p1` - `cpt-cf-bss-rating-fr-committed-usage`
  - [ ] `p1` - `cpt-cf-bss-rating-fr-commitment-drawdown`
  - [ ] `p1` - `cpt-cf-bss-rating-fr-reservation-consumption-flavor`
  - [ ] `p1` - `cpt-cf-bss-rating-fr-capacity-charge`
  - [ ] `p2` - `cpt-cf-bss-rating-fr-coupon-application-order`
  - [ ] `p2` - `cpt-cf-bss-rating-fr-coupon-stacking`
  - [ ] `p1` - `cpt-cf-bss-rating-fr-multi-currency`
  - [ ] `p1` - `cpt-cf-bss-rating-fr-fx-policy`
  - [ ] `p2` - `cpt-cf-bss-rating-contract-finance-fx-input`
  - [ ] `p2` - `cpt-cf-bss-rating-contract-promotions-coupon`
  - [ ] `p1` - `cpt-cf-bss-rating-contract-contracts-input`

  `cpt-cf-bss-rating-fr-priceoverlay-scope-mapping` and `cpt-cf-bss-rating-fr-overlay-stacking` are
  **superseded** (T-D-73): Pricing has no price overlays. The other requirements above are dormant:
  they have no source and fail closed (PRD §5.3 release gates).

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-bss-rating-principle-fail-closed`
  - [ ] `p1` - `cpt-cf-bss-rating-principle-fixed-rule-order`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-bss-rating-constraint-native-currency-launch`
  - [ ] `p1` - `cpt-cf-bss-rating-constraint-exact-no-rounding`
  - [ ] `p1` - `cpt-cf-bss-rating-constraint-domain-boundaries`

- **Domain Model Entities**:
  - `OverlaidLine`, `ReservationMatch`, `CommitmentPoolInput`, `CouponSnapshot`, `CurrencyRoles`
  - [ ] `p1` - `cpt-cf-bss-rating-entity-model-ovl`
  - [ ] `p1` - `cpt-cf-bss-rating-entity-model-cmt`
  - [ ] `p2` - `cpt-cf-bss-rating-entity-model-cpn`
  - [ ] `p1` - `cpt-cf-bss-rating-entity-model-fx`

- **Design Components**:

  - [ ] `p1` - `cpt-cf-bss-rating-component-overlays-precedence`
  - [ ] `p1` - `cpt-cf-bss-rating-component-commitments-reservations`
  - [ ] `p2` - `cpt-cf-bss-rating-component-coupons`
  - [ ] `p1` - `cpt-cf-bss-rating-component-currency-fx`
  - [ ] `p1` - `cpt-cf-bss-rating-component-overlays-precedence-ovl`
  - [ ] `p1` - `cpt-cf-bss-rating-component-commitment-evaluator-cmt`
  - [ ] `p2` - `cpt-cf-bss-rating-component-coupon-evaluator-cpn`
  - [ ] `p1` - `cpt-cf-bss-rating-component-conversion-fx`

- **API**:
  - In-process step evaluators inside `rating-core` only.

- **Sequences**:

  - [ ] `p3` - `cpt-cf-bss-rating-seq-commitment-cascade` — dormant (R-11). This feature owns the slot-4b semantics; the dormant `BalanceEffectPublisher` that appends effects in the rater transaction is feature 07's, and the re-evaluation cascade on a new pool version is feature 08's, once a source exists.

- **Data**:

  - None (`rating-core`). Namespace data statements: `cpt-cf-bss-rating-db-none-ovl`, `cpt-cf-bss-rating-db-none-cmt`, `cpt-cf-bss-rating-db-none-cpn`, `cpt-cf-bss-rating-db-none-fx`.

- **Split of the input contracts**: this feature owns how the step-4 slots 4a–4d evaluate the contract, coupon and
  FX inputs. Freezing those inputs into `EvaluationInput` is feature 07's `ContextAssembler`, and the
  outbound `CommitmentBalanceEffect` is feature 07's dormant `BalanceEffectPublisher`.

- **Detailed contract coverage**: namespaces [04](DESIGN.md#contract-04)–[07](DESIGN.md#contract-07);
  [11 §4.4](DESIGN.md#contract-11-4-4), [11 §4.5](DESIGN.md#contract-11-4-5), [11 §4.9](DESIGN.md#contract-11-4-9).
  - **Interfaces**: `cpt-cf-bss-rating-interface-stack-overlays-ovl`, `cpt-cf-bss-rating-interface-step6-evaluator-cmt`, `cpt-cf-bss-rating-interface-step7-evaluator-cpn`, `cpt-cf-bss-rating-interface-convert-fx`, `cpt-cf-bss-rating-interface-contracts-input-cc`
  - **Deployment**: `cpt-cf-bss-rating-topology-ovl`, `cpt-cf-bss-rating-topology-cmt`, `cpt-cf-bss-rating-topology-cpn`, `cpt-cf-bss-rating-topology-fx`, `cpt-cf-bss-rating-tech-stack-ovl`, `cpt-cf-bss-rating-tech-stack-cmt`, `cpt-cf-bss-rating-tech-stack-cpn`, `cpt-cf-bss-rating-tech-stack-fx`

- **Phase**: 0 for the launch posture (empty inputs, fail-closed rules); each step's source-backed
  behaviour activates with its upstream ask (§3.1).

---

### 2.4 [Usage Capture and Normalization](features/04-usage-intake.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-feature-usage-intake`

- **Purpose**: Usage is financial input that the collector keeps only for its retention floor.
  Capturing every entry complete before interpreting it, advancing the feed cursor only in the
  transaction that stores what it covers, and recording a source loss when the feed cannot prove
  continuity is what makes usage lossless and replayable inside Rating.

- **Depends On**: `cpt-cf-bss-rating-feature-foundation`

- **Scope**:
  - `UsageFeedReader` per canonical source identity with the checkpoint CAS and fence, the Cluster lock
    and backoff; `read_usage_feed` paging.
  - Raw capture with `raw_sha256`, dedup on the collector id and natural key, capture rejects for
    unidentifiable entries.
  - `UsageNormalizer`: collector entry → Rating usage mapping, usage type declarations, `SUM`-only
    chargeability, `dimension_key` population (R-16), rejections into the exception register with
    retry from the stored raw entry.
  - Invalidation intake: capture, withdrawal of the target through the contribution state machine.
  - `SourceLossTracker`: `CursorBeyondRetention`, unidentifiable entries, scope widening.
  - Archive manifests for cold-tiered capture partitions.
- **Out of scope**:
  - Attribution, counters and window placement (feature 05); usage evaluation (feature 07).
  - Reconciliation that narrows or resolves a source loss (feature 11).

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-bss-rating-fr-dimension-population-contract`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-bss-rating-principle-capture-before-interpret`
  - [ ] `p1` - `cpt-cf-bss-rating-principle-copy-what-you-rate`
  - [ ] `p1` - `cpt-cf-bss-rating-principle-evidence-before-final`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-bss-rating-constraint-in-process-contracts`
  - [ ] `p1` - `cpt-cf-bss-rating-constraint-authoritative-dedup-ing`
  - [ ] `p1` - `cpt-cf-bss-rating-constraint-entries-not-aggregates-ing`

- **Domain Model Entities**:
  - Usage capture, Source checkpoint, Source loss (DESIGN §3.1)
  - [ ] `p1` - `cpt-cf-bss-rating-entity-model-ing`

- **Design Components**:

  - [ ] `p1` - `cpt-cf-bss-rating-component-usage-ingestion`

- **API**:
  - Consumes `read_usage_feed` (UPSTREAM_REQS §2.1). No Rating endpoint ingests usage.

- **Sequences**:

  - [ ] `p1` - `cpt-cf-bss-rating-seq-ingest-usage` (capture and interpretation half)
  - [ ] `p1` - `cpt-cf-bss-rating-seq-correction` (invalidation capture, step 2)

- **Data**:

  - [ ] `p1` - `cpt-cf-bss-rating-dbtable-feed-cursor`
  - [ ] `p1` - `cpt-cf-bss-rating-dbtable-usage-record`
  - [ ] `p1` - `cpt-cf-bss-rating-dbtable-source-loss`

- **Detailed contract coverage**: namespace [12](DESIGN.md#contract-12); its procedures
  [12 §4.4](features/04-usage-intake.md#contract-12-4-4)–[§4.6](features/04-usage-intake.md#contract-12-4-6).
  - **Interfaces**: `cpt-cf-bss-rating-interface-ingest-ing`
  - **Data**: `cpt-cf-bss-rating-db-ingestion-ing`, `cpt-cf-bss-rating-db-tiering-ing`
  - **Deployment**: `cpt-cf-bss-rating-topology-ing`, `cpt-cf-bss-rating-tech-stack-ing`

- **Phase**: 1 against the feed fake; production with `…-upreq-usage-feed-v1`.

---

### 2.5 [Attribution and Window Counters](features/05-attribution-counters.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-feature-attribution-counters`

- **Purpose**: A usage record is billable only once it is attributed to a subscription line and placed
  in a window and slice. Doing that through a local projection and versioned counters — never a
  per-record lookup, never a guessed subscription — is what keeps `Q` exact, recomputable and
  serialized per counter row.

- **Depends On**: `cpt-cf-bss-rating-feature-foundation`, `cpt-cf-bss-rating-feature-evaluation-core`, `cpt-cf-bss-rating-feature-usage-intake`

- **Scope**:
  - `AttributionProjector`: segment intake through the inbox, gap-free `lifecycle_seq`, `segments_since`
    fill; usage scope and coverage declaration intake as stored evidence.
  - `Attributor` and the boundary-split rule (T-D-53); `WindowResolver`; usage-policy projections (`bss_rating__usage_policy_projection`, formerly meter specs) naming each derived meter's raw inputs (T-D-76).
  - `CounterMaterializer`: the `count` transition, `q_version`, provisional child insertion and
    enqueue, decrement at recorded coordinates on withdrawal.
  - Slice layouts (from `split_points`), re-materialization, composite inputs, counter integrity
    recompute.
- **Out of scope**:
  - Deciding finality from the evidence (feature 07); creating expected children on schedule
    (feature 06).

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-bss-rating-fr-tier-aggregation-window`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-bss-rating-principle-fail-closed`
  - [ ] `p1` - `cpt-cf-bss-rating-principle-adopt-the-sor`
  - [ ] `p1` - `cpt-cf-bss-rating-principle-single-writer-qst`
  - [ ] `p1` - `cpt-cf-bss-rating-principle-versioned-q-qst`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-bss-rating-constraint-core-never-writes-qst`
  - [ ] `p1` - `cpt-cf-bss-rating-constraint-no-fabricated-proof-qst`

- **Domain Model Entities**:
  - Window counter, attribution segment projection, usage scope, coverage declaration
  - [ ] `p1` - `cpt-cf-bss-rating-entity-model-qst`

- **Design Components**:

  - [ ] `p1` - `cpt-cf-bss-rating-component-q-store`

- **API**:
  - Consumes `AttributionSegmentChanged` / `segments_since`, `UsageScopeSealed`,
    `MeterCoverageDeclared` through the inbox (UPSTREAM_REQS §2.2, §2.6).

- **Sequences**:

  - [ ] `p1` - `cpt-cf-bss-rating-seq-ingest-usage` (`count` and child enqueue)

- **Data**:

  - [ ] `p1` - `cpt-cf-bss-rating-dbtable-attribution-segment`
  - [ ] `p1` - `cpt-cf-bss-rating-dbtable-window-counter`
  - [ ] `p1` - `cpt-cf-bss-rating-dbtable-window-layout`
  - [ ] `p1` - `cpt-cf-bss-rating-dbtable-meter-spec`

- **Detailed contract coverage**: namespace [13](DESIGN.md#contract-13) except 13 §4.7 (feature 07);
  procedures [13 §4.4](features/05-attribution-counters.md#contract-13-4-4) and
  [13 §4.6](features/05-attribution-counters.md#contract-13-4-6).
  - **Interfaces**: `cpt-cf-bss-rating-interface-q-store-qst`
  - **Data**: `cpt-cf-bss-rating-db-q-qst`
  - **Deployment**: `cpt-cf-bss-rating-topology-qst`, `cpt-cf-bss-rating-tech-stack-qst`

- **Phase**: 1 against the Subscriptions fakes; production with `…-upreq-subscriptions-attribution-segments`.

---

### 2.6 [Commercial Facts and Window Scheduling](features/06-fact-scheduling.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-feature-fact-scheduling`

- **Purpose**: A commercial fact authorizes rating; Rating, not Subscriptions, derives and schedules the
  child windows beneath it and wakes every child whose state can change by time alone. A durable
  cursor and durable wake-ups are what let a month of 744 hourly windows progress, and catch up after
  an outage, without any further upstream event.

- **Depends On**: `cpt-cf-bss-rating-feature-foundation`, `cpt-cf-bss-rating-feature-evaluation-core`

- **Scope**:
  - `FactIntake`: inbox acceptance of facts, fact versions and heads, the MIGRATION mappings for
    today's recurring fact and the derived usage parent (R-20), launch-gated window policies.
  - Rating-owned `FinalizationPolicy` resolution and pinning (T-D-59), the seller and platform policy
    write paths.
  - `WindowScheduler`: expected children per `window_geometry`, the `next_due_window_start` cursor,
    catch-up.
  - `ChildWakeScanner`: `next_check_at` and `rollup_due_at` wake-ups (T-D-68); work queue coalescing.
  - The fact watchdog against `period_facts` (TARGET) or the `rating_fact_missing` alarm (CURRENT).
- **Out of scope**:
  - Evaluating children and the evidence gate (feature 07); roll-up (feature 09).
  - Deciding any commercial period: Subscriptions owns the WHEN (T-D-33).

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-bss-rating-contract-subscriptions-input`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-bss-rating-principle-adopt-the-sor`
  - [ ] `p1` - `cpt-cf-bss-rating-principle-evidence-before-final`
  - [ ] `p1` - `cpt-cf-bss-rating-principle-idempotent-tick-syn`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-bss-rating-constraint-no-commercial-clock-syn`
  - [ ] `p1` - `cpt-cf-bss-rating-constraint-synthesis-only-syn`

- **Domain Model Entities**:
  - Fact projection, Child window, Window schedule, Finalization policy (DESIGN §3.1)
  - [ ] `p1` - `cpt-cf-bss-rating-entity-model-syn`

- **Design Components**:

  - [ ] `p1` - `cpt-cf-bss-rating-component-unit-synthesis`

- **API**:
  - REST `bss_rating.finalization_policies.list`, `.create`, `bss_rating.platform_finalization_policies.create`
    (`cpt-cf-bss-rating-interface-operator-rest`).
  - Consumes commercial facts (UPSTREAM_REQS §2.6).

- **Sequences**:

  - [ ] `p1` - `cpt-cf-bss-rating-seq-period-fact`
  - [ ] `p1` - `cpt-cf-bss-rating-seq-usage-parent`

- **Data**:

  - [ ] `p1` - `cpt-cf-bss-rating-dbtable-fact`
  - [ ] `p1` - `cpt-cf-bss-rating-dbtable-window-schedule`
  - [ ] `p1` - `cpt-cf-bss-rating-dbtable-finalization-policy`
  - [ ] `p1` - `cpt-cf-bss-rating-dbtable-child-window`

- **Detailed contract coverage**: [14 §1.1](DESIGN.md#contract-14-1-1)–[§3.8](DESIGN.md#contract-14-3-8),
  [14 §4.1](DESIGN.md#contract-14-4-1), [14 §4.2](DESIGN.md#contract-14-4-2); procedures
  [14 §3.6 (fact intake, scheduler, wake-ups)](features/06-fact-scheduling.md#contract-14-3-6-f06) and
  [14 §4.4](features/06-fact-scheduling.md#contract-14-4-4).
  - **Interfaces**: `cpt-cf-bss-rating-interface-synthesis-syn`
  - **Data**: `cpt-cf-bss-rating-db-synthesis-syn`
  - **Deployment**: `cpt-cf-bss-rating-topology-syn`, `cpt-cf-bss-rating-tech-stack-syn`

- **Phase**: 1 against the Subscriptions fakes; production with `…-upreq-subscriptions-versioned-facts`
  and `…-upreq-subscriptions-usage-fact`.

---

### 2.7 [Child Evaluation and Finalization](features/07-child-evaluation.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-feature-child-evaluation`

- **Purpose**: The rater is the only writer of rated money. One transaction per child — locked row,
  input-generation CAS, binding of record and engine re-derived under the lock, inputs read only from stored copies,
  a result final only on proven evidence — is what makes every result exact, current and replayable.

- **Depends On**: `cpt-cf-bss-rating-feature-foundation`, `cpt-cf-bss-rating-feature-evaluation-core`, `cpt-cf-bss-rating-feature-price-adjustments`, `cpt-cf-bss-rating-feature-attribution-counters`, `cpt-cf-bss-rating-feature-fact-scheduling`

- **Scope**:
  - `EvidenceGate` (finalization gate T-D-52, source-loss block T-D-62) and scope/coverage evidence
    acquisition; pending reasons and `next_check_at`.
  - `ContextAssembler`: stored bindings (`PricingBindingStore`, `bss_rating__pricing_binding`),
    derived declarations (`bss_rating__derived_declaration`), subscription copies, the window-group
    context (T-D-67), input manifest and `input_digest` (T-D-73, T-D-76).
  - `Rater`: provisional and final results, binding of record and engine of record, `EngineRegistry`
    dispatch and start-up verification, closing met re-rate targets, roll-up enqueue.
  - `OutcomeMapper`: window results, content-addressed snapshots, group contexts; dormant
    `BalanceEffectPublisher`.
- **Out of scope**:
  - Choosing the re-rate target or enumerating a run (feature 08); parent results and delivery
    (feature 09).

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-bss-rating-fr-snapshot-carry`
  - [ ] `p1` - `cpt-cf-bss-rating-nfr-resilience`
  - [ ] `p1` - `cpt-cf-bss-rating-contract-rating-handoff`
  - [ ] `p1` - `cpt-cf-bss-rating-contract-pricing-readmodel`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-bss-rating-principle-copy-what-you-rate`
  - [ ] `p1` - `cpt-cf-bss-rating-principle-append-only-money`
  - [ ] `p1` - `cpt-cf-bss-rating-principle-evidence-before-final`
  - [ ] `p1` - `cpt-cf-bss-rating-principle-freeze-then-invoke-syn`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-bss-rating-constraint-pin-discipline-syn`
  - [ ] `p1` - `cpt-cf-bss-rating-constraint-stateless-hot-path`
  - [ ] `p1` - `cpt-cf-bss-rating-constraint-record-not-compute-rob`

- **Domain Model Entities**:
  - Window result, Rating snapshot, Window-group context, Engine generation (DESIGN §3.1)
  - [ ] `p1` - `cpt-cf-bss-rating-entity-model-rob`

- **Design Components**:

  - [ ] `p1` - `cpt-cf-bss-rating-component-rated-output`
  - [ ] `p1` - `cpt-cf-bss-rating-component-boundary-surface-cc`

- **API**:
  - REST `bss_rating.windows.list`, `bss_rating.snapshots.get` (read; `cpt-cf-bss-rating-interface-operator-rest`).
  - Consumes `PricingReadV1::{resolve, price}` (T-D-73, as `bss-rating.system` with pricing `plan:read` / `price:read`), the Products derived-declaration read (R-31) and subscription versions (UPSTREAM_REQS §2.4, §2.5, §2.6).

- **Sequences**:

  - [ ] `p1` - `cpt-cf-bss-rating-seq-evaluate-tariff`

- **Data**:

  - [ ] `p1` - `cpt-cf-bss-rating-dbtable-rated-version`
  - [ ] `p1` - `cpt-cf-bss-rating-dbtable-group-context`
  - [ ] `p1` - `cpt-cf-bss-rating-dbtable-snapshot`
  - [ ] `p1` - `cpt-cf-bss-rating-dbtable-catalog-document`
  - [ ] `p1` - `cpt-cf-bss-rating-dbtable-subscription-version`
  - [ ] `p1` - `cpt-cf-bss-rating-dbtable-engine-generation`

  `cpt-cf-bss-rating-dbtable-catalog-document` now defines `bss_rating__pricing_binding` (and
  `bss_rating__derived_declaration`), replacing the catalog-document copy (T-D-73).

- **Detailed contract coverage**: [11 §4.1](DESIGN.md#contract-11-4-1)–[§4.3](DESIGN.md#contract-11-4-3),
  [13 §4.7](DESIGN.md#contract-13-4-7), [14 §4.5](DESIGN.md#contract-14-4-5), [14 §4.6](DESIGN.md#contract-14-4-6),
  [15 §4.1](DESIGN.md#contract-15-4-1), [15 §4.2](DESIGN.md#contract-15-4-2), [15 §4.4](DESIGN.md#contract-15-4-4),
  [15 §4.5](DESIGN.md#contract-15-4-5); procedures [11 §3.6](features/07-child-evaluation.md#contract-11-3-6),
  [14 §3.6 (evaluate a child)](features/07-child-evaluation.md#contract-14-3-6), [14 §4.3](features/07-child-evaluation.md#contract-14-4-3),
  [15 §3.6 (persist an outcome)](features/07-child-evaluation.md#contract-15-3-6).
  - **Interfaces**: `cpt-cf-bss-rating-interface-rating-handoff-cc`, `cpt-cf-bss-rating-interface-pricing-readmodel-cc`, `cpt-cf-bss-rating-interface-rated-output-rob`
  - **Data**: `cpt-cf-bss-rating-db-rated-output-rob`
  - **Deployment**: `cpt-cf-bss-rating-topology-rob`, `cpt-cf-bss-rating-tech-stack-rob`

- **Phase**: 1 against the pricing and Subscriptions fakes; production with the asks in §3.1 (facts
  carrying pins, the Products declaration read R-31).

---

### 2.8 [Corrections, Replay and Administrative Re-rate](features/08-corrections-rerate.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-feature-corrections-rerate`

- **Purpose**: A correction must reprice exactly what changed with the prices and engine the window
  was finalized with, and only an approved administrative re-rate may move them. Making the run
  durable — a frozen target per run and per child — is what stops a queue coalesce, a crash or a
  stale worker from losing or reverting an approved repricing.

- **Depends On**: `cpt-cf-bss-rating-feature-child-evaluation`, `cpt-cf-bss-rating-feature-rollup-delivery`

- **Scope**:
  - Correction propagation: an input change (late or invalidated usage, attribution, scope or
    coverage, fact version, plan change) bumps `input_generation` and re-evaluates with the binding and
    engine of record (it resolves again only for a re-rate with `rebind` or a changed pricing query of
    the fact version: revision, period or pins); reversal math as re-evaluation.
  - `RerateService`: `RatingRunControlV1`, `POST /reratings` with `Idempotency-Key`, frozen target,
    resumable enumeration, rate-limited enqueue, cancel.
  - Replay of a recorded revision and the replay determinism check.
  - The superseded pricing-change flow as it applies to re-rate semantics; the dormant pool-version
    cascade.
- **Out of scope**:
  - Computing any monetary delta: Billing derives it (T-D-50).
  - Period routing: Rating has none (T-D-50).

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-bss-rating-fr-idempotency`
  - [ ] `p1` - `cpt-cf-bss-rating-fr-separation`
  - [ ] `p1` - `cpt-cf-bss-rating-fr-posted-period-protection`
  - [ ] `p2` - `cpt-cf-bss-rating-fr-late-arriving-usage-reresolve`
  - [ ] `p2` - `cpt-cf-bss-rating-fr-usage-corrections`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-bss-rating-principle-absolute-results`
  - [ ] `p1` - `cpt-cf-bss-rating-principle-pinned-replay-rtr`
  - [ ] `p1` - `cpt-cf-bss-rating-principle-same-math-rtr`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-bss-rating-constraint-posted-immutability`
  - [ ] `p1` - `cpt-cf-bss-rating-constraint-posted-immutability-rtr`

- **Domain Model Entities**:
  - Re-rate run and target (DESIGN §3.1)
  - [ ] `p1` - `cpt-cf-bss-rating-entity-model-rtr`

- **Design Components**:

  - [ ] `p2` - `cpt-cf-bss-rating-component-retroactivity-corrections`
  - [ ] `p1` - `cpt-cf-bss-rating-component-correction-wrapper-rtr`

- **API**:
  - SDK `RatingRunControlV1::{request_rerate, get_rerate_run}`; REST `bss_rating.reratings.create`,
    `.get`, `.cancel` (`cpt-cf-bss-rating-interface-rating-client`, `cpt-cf-bss-rating-interface-operator-rest`).

- **Sequences**:

  - [ ] `p1` - `cpt-cf-bss-rating-seq-correction` (re-evaluation and revision half)
  - [ ] `p2` - `cpt-cf-bss-rating-seq-plan-change`
  - [ ] `p2` - `cpt-cf-bss-rating-seq-pricing-change` (superseded on `main`; re-rate semantics kept)
  - [ ] `p2` - `cpt-cf-bss-rating-seq-admin-rerate`

- **Data**:

  - [ ] `p2` - `cpt-cf-bss-rating-dbtable-rerate-run`

- **Detailed contract coverage**: namespace [08](DESIGN.md#contract-08); procedures
  [08 §3.6](features/08-corrections-rerate.md#contract-08-3-6) and the [08 §4.5 vectors](features/08-corrections-rerate.md#contract-08-4-5).
  - **Interfaces**: `cpt-cf-bss-rating-interface-delta-envelope-rtr`
  - **Data**: `cpt-cf-bss-rating-db-none-rtr`
  - **Deployment**: `cpt-cf-bss-rating-topology-rtr`, `cpt-cf-bss-rating-tech-stack-rtr`

- **Phase**: 2.

---

### 2.9 [Parent Roll-up and Billing Delivery](features/09-rollup-delivery.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-feature-rollup-delivery`

- **Purpose**: Billing needs one complete, exact, absolute result per parent fact revision, built only
  from current child results, and a delivery it can always recover by pull. Rolling up under the fact
  lock with a freshness check and publishing through an outbox is what guarantees a stale or partial
  result never reaches Billing.

- **Depends On**: `cpt-cf-bss-rating-feature-foundation`, `cpt-cf-bss-rating-feature-child-evaluation`

- **Scope**:
  - `ParentRollup`: completeness against the expected manifest, freshness (T-D-64), the served-period
    wait, no-op detection, `line_key` / `invoice_line_key` identities, exact sums.
  - `BillableItemDeliveryV1`, the delivery outbox and `bss_rating__delivery_feed`, `DeliveryPublisher`
    (event path disabled by default).
  - `RatingRunReadV1::{find_runs, get_run, window_results, deliveries_since}`; REST `runs.get`.
  - `BillingHintSink` (observational only).
- **Out of scope**:
  - Rounding, the invoice line key, period state, notes and posting — Billing's (T-D-50, T-D-51, R-32).

- **Requirements Covered**:

  - [ ] `p2` - `cpt-cf-bss-rating-contract-billing-periodstate`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-bss-rating-principle-absolute-results`
  - [ ] `p1` - `cpt-cf-bss-rating-principle-append-only-money`
  - [ ] `p1` - `cpt-cf-bss-rating-principle-idempotent-delivery-bhf`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-bss-rating-constraint-exact-no-rounding`
  - [ ] `p1` - `cpt-cf-bss-rating-constraint-posted-immutability`
  - [ ] `p1` - `cpt-cf-bss-rating-constraint-complete-or-nothing-rob`

- **Domain Model Entities**:
  - Parent result, Delivery (DESIGN §3.1)
  - [ ] `p1` - `cpt-cf-bss-rating-entity-model-bhf`

- **Design Components**:

  - [ ] `p1` - `cpt-cf-bss-rating-component-billing-handoff`

- **API**:
  - SDK `RatingRunReadV1` and `BillableItemDeliveryV1` (`cpt-cf-bss-rating-interface-rating-client`);
    delivery event `gts.cf.bss.rating.billable_item_delivery.v1~` (`cpt-cf-bss-rating-interface-events`, disabled);
    REST `bss_rating.runs.get`.

- **Sequences**:

  - [ ] `p1` - `cpt-cf-bss-rating-seq-parent-rollup`
  - [ ] `p2` - `cpt-cf-bss-rating-seq-period-close` (Billing-owned; hint only)

- **Data**:

  - [ ] `p1` - `cpt-cf-bss-rating-dbtable-fact-result`
  - [ ] `p1` - `cpt-cf-bss-rating-dbtable-charge-feed`

- **Detailed contract coverage**: [15 §4.3](DESIGN.md#contract-15-4-3), namespace [16](DESIGN.md#contract-16)
  except 16 §4.3–§4.5, [11 §4.6](DESIGN.md#contract-11-4-6), [11 §4.7](DESIGN.md#contract-11-4-7);
  procedures [16 §3.6](features/09-rollup-delivery.md#contract-16-3-6),
  [15 §3.6 (revision example)](features/09-rollup-delivery.md#contract-15-3-6-f09), [15 §4.6 vectors](features/09-rollup-delivery.md#contract-15-4-6).
  - **Interfaces**: `cpt-cf-bss-rating-interface-billing-handoff-bhf`
  - **Data**: `cpt-cf-bss-rating-db-billing-handoff-bhf`
  - **Deployment**: `cpt-cf-bss-rating-topology-bhf`, `cpt-cf-bss-rating-tech-stack-bhf`

- **Phase**: 1 for the pull feed; event publication with `…-upreq-event-contracts`; consumption with
  `…-upreq-billing-delivery-consumer`.

---

### 2.10 [Pre-Purchase Order Evaluation](features/10-order-evaluation.md) - MEDIUM

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-feature-order-evaluation`

- **Purpose**: Orders Lifecycle needs resolved totals before a subscription exists. Serving them from
  the same arithmetic, through a separate DTO that persists nothing, gives Orders exact numbers without
  making an order a rating input.

- **Depends On**: `cpt-cf-bss-rating-feature-foundation`, `cpt-cf-bss-rating-feature-evaluation-core`, `cpt-cf-bss-rating-feature-price-adjustments`

- **Scope**:
  - `OrderEvaluator` and `OrderEvaluationV1::evaluate`: authorization (`evaluation × evaluate`),
    the request's bindings priced as given (no pricing read; T-D-79), `evaluate_order`, usage excluded
    from TCV (T-D-54).
- **Out of scope**:
  - Persistence, idempotency keys, usage estimation; the order's own `OrderPin` and stored totals (Orders Lifecycle).

- **Requirements Covered**:

  - [ ] `p2` - `cpt-cf-bss-rating-fr-pre-purchase-evaluation`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-bss-rating-principle-pure-function-core`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-bss-rating-constraint-exact-no-rounding`

- **Domain Model Entities**:
  - `OrderEvaluationRequest`, `OrderEvaluation`

- **Design Components**:

  - [ ] `p1` - `cpt-cf-bss-rating-component-consumer-contracts` (order-evaluation surface)

- **API**:
  - SDK `OrderEvaluationV1::evaluate` (`cpt-cf-bss-rating-interface-rating-client`).

- **Sequences**:

  - None in DESIGN §3.6; the flow is in the feature.

- **Data**:

  - None (persists nothing, T-D-54).

- **Detailed contract coverage**: [11 §4.10](DESIGN.md#contract-11-4-10).
  - **Interfaces**: `cpt-cf-bss-rating-interface-order-evaluation-cc`

- **Phase**: 1 against the pricing fake; production after R-24 (`…-upreq-orders-evaluation-dto`; Orders confirms T-D-79).

---

### 2.11 [Operations, Reconciliation and Data Lifecycle](features/11-operations.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-feature-operations`

- **Purpose**: The pipeline's guarantees hold only if someone can see and repair what failed: open
  exceptions and quarantined facts, source losses, missed facts, counter drift, lost deliveries and
  non-determinism. Retention, holds and archive integrity keep seven years of results replayable, and
  the NFR acceptance gates prove the sizing before launch.

- **Depends On**: `cpt-cf-bss-rating-feature-usage-intake`, `cpt-cf-bss-rating-feature-attribution-counters`, `cpt-cf-bss-rating-feature-fact-scheduling`, `cpt-cf-bss-rating-feature-child-evaluation`, `cpt-cf-bss-rating-feature-corrections-rerate`, `cpt-cf-bss-rating-feature-rollup-delivery`

- **Scope**:
  - Operator REST: exceptions list/retry, inbox release, source losses list/resolve, retention holds.
  - `Reconciler` checks of DESIGN §4.6 and their repairs; source-loss narrowing and resolution.
  - Observability: the metric, log, span and alert set of DESIGN §4.7.
  - Data lifecycle of DESIGN §4.12: retention classes, holds, archive integrity, tenant deletion.
  - Backpressure, bounded replay, cold start; NFR verification and the load-test acceptance gates of
    DESIGN §4.9.
- **Out of scope**:
  - Repricing (feature 08); changing any upstream source.

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-bss-rating-nfr-throughput-latency`
  - [ ] `p1` - `cpt-cf-bss-rating-nfr-horizontal-scale`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-bss-rating-principle-evidence-before-final`
  - [ ] `p1` - `cpt-cf-bss-rating-principle-lanes-bhf`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-bss-rating-constraint-domain-boundaries`

- **Domain Model Entities**:
  - Exception, Source loss, Retention hold (DESIGN §3.1, §3.7)

- **Design Components**:

  - [ ] `p1` - `cpt-cf-bss-rating-component-billing-handoff` (operations half)

- **API**:
  - REST `bss_rating.exceptions.list`, `.retry`, `bss_rating.inbox.release`, `bss_rating.source_losses.list`,
    `.resolve`, `bss_rating.retention_holds.list`, `.create`, `.release` (`cpt-cf-bss-rating-interface-operator-rest`).

- **Sequences**:

  - None separately identified in DESIGN §3.6; the reconciliation checks are DESIGN §4.6.

- **Data**:

  - `bss_rating__retention_hold`, `bss_rating__usage_archive_manifest` (within `cpt-cf-bss-rating-dbtable-rerate-run` and `cpt-cf-bss-rating-dbtable-usage-record`).

- **Detailed contract coverage**: [16 §4.3](DESIGN.md#contract-16-4-3); procedures
  [16 §4.4](features/11-operations.md#contract-16-4-4) and [16 §4.5](features/11-operations.md#contract-16-4-5).

- **Phase**: 2/3; the load-test gates of DESIGN §4.9 precede implementation lock.

---

## 3. Feature Dependencies

```text
cpt-cf-bss-rating-feature-foundation          cpt-cf-bss-rating-feature-evaluation-core
    │                                              │
    ├─→ cpt-cf-bss-rating-feature-usage-intake      ├─→ cpt-cf-bss-rating-feature-price-adjustments
    │       └─→ cpt-cf-bss-rating-feature-attribution-counters  (also needs evaluation-core)
    ├─→ cpt-cf-bss-rating-feature-fact-scheduling   (also needs evaluation-core)
    │
    └─→ cpt-cf-bss-rating-feature-child-evaluation  (needs evaluation-core, price-adjustments,
            │                                        attribution-counters, fact-scheduling)
            └─→ cpt-cf-bss-rating-feature-rollup-delivery
                    └─→ cpt-cf-bss-rating-feature-corrections-rerate
    cpt-cf-bss-rating-feature-order-evaluation      (foundation, evaluation-core, price-adjustments)
    cpt-cf-bss-rating-feature-operations            (after every pipeline feature)
```

The table is the build-order authority. Numeric prefixes follow implementation order; all features but
order evaluation are HIGH because each carries a `p1` obligation, and phases express sequencing, not
priority.

| Feature | Phase | Depends on |
|---------|-------|------------|
| [01 foundation](features/01-foundation.md) | 0 | platform prerequisites (§3.1) |
| [02 evaluation-core](features/02-evaluation-core.md) | 0 | — |
| [03 price-adjustments](features/03-price-adjustments.md) | 0 | 02 |
| [04 usage-intake](features/04-usage-intake.md) | 1 | 01 |
| [05 attribution-counters](features/05-attribution-counters.md) | 1 | 01, 02, 04 |
| [06 fact-scheduling](features/06-fact-scheduling.md) | 1 | 01, 02 |
| [07 child-evaluation](features/07-child-evaluation.md) | 1 | 01, 02, 03, 05, 06 |
| [09 rollup-delivery](features/09-rollup-delivery.md) | 1 | 01, 07 |
| [10 order-evaluation](features/10-order-evaluation.md) | 1 | 01, 02, 03 |
| [08 corrections-rerate](features/08-corrections-rerate.md) | 2 | 07, 09 |
| [11 operations](features/11-operations.md) | 2/3 | 04, 05, 06, 07, 08, 09 |

**Dependency rationale and parallel work:**

- `evaluation-core` needs nothing outside its own golden vectors: it is the first thing built (build
  step 1 below). `foundation` can proceed in parallel.
- `price-adjustments` extends the core's step pipeline; it can proceed alongside the pipeline features.
- `usage-intake` and `fact-scheduling` are independent of each other after `foundation`;
  `attribution-counters` needs captured records from `usage-intake` and `split_points` /
  `window_geometry` from the core.
- `child-evaluation` needs children (`fact-scheduling`), counters and evidence (`attribution-counters`)
  and a complete step pipeline (core and adjustments).
- `rollup-delivery` consumes current child results; `corrections-rerate` needs both, because a
  correction's acceptance is a new parent revision.
- `order-evaluation` shares only the core and the foundation's SDK and authorization, so it can be
  built in parallel with the pipeline once R-24 is settled.
- `operations` observes and repairs every pipeline feature, so it comes last; its load-test gates are
  run before implementation lock.

**Build and production-enablement order** (formerly DESIGN §4.10 rollout):

1. **Now**: build `rating-core` (generation 1) with Rating's own golden vectors checked against
   pricing's `amount_for` oracle and seam fixtures (the joint `bss-fixtures` corpus was deleted); add the Atlas
   arithmetic fixtures (F02, F03, F10, F19, F23–F29, F32) and the acceptance vectors of the features as
   `rating-core` and pipeline tests. No dependency blocks this.
2. **With test fakes** for C01 (`PricingReadV1` over pricing's golden contracts), C04 and C05 (Atlas "Rating can start"):
   capture, inbox, scheduler, wake scanner, evidence gate, roll-up, delivery, `OrderEvaluationV1`.
   Fakes never stand in for a production contract.
3. **After R-03** (facts carrying pins, plan revision, quantity and `BillingTerms`): recurring facts
   end to end (DESIGN Flow C) — no usage dependency; the pricing adapter itself is designed (T-D-73).
4. **After R-01 + R-25 + R-21 + R-31**: usage children (derived meters with `Sum` inputs, all launch
   model kinds, `CalendarHour` and `BillingCycle` windows).
5. **After R-06 / R-07 / R-11 / R-19**: `Peak` / `TimeWeighted` derived inputs, FX, contract
   overlays / commitments / coupons, one-time rating.

### 3.1 Upstream and release prerequisites

Each feature can be built and accepted against the contract fakes of
[`01-foundation`](features/01-foundation.md); production enablement waits on the asks below, all open
in [UPSTREAM_REQS.md](UPSTREAM_REQS.md). An ask is never equated with an implemented seam.

| Feature | Production prerequisite (UPSTREAM_REQS ID, DECISIONS row) |
|---|---|
| 01 foundation | `…-upreq-cluster-coordination-backend` (R-27) — **mandatory before the first deployment**; event paths: `…-upreq-event-contracts` |
| 02 evaluation-core | none for the golden vectors; live bindings arrive through fact intake (T-D-73); `Peak` / `TimeWeighted` inputs `…-upreq-level-meter-rule` (R-06); non-empty `dimension_key` `…-upreq-dimension-encoding` (R-16); derived declarations (R-31); narrower bounds `…-upreq-pricing-numeric-limits`, `…-upreq-products-derived-output-limits` (R-33, not blocking) |
| 03 price-adjustments | `…-upreq-contracts-inputs`, `…-upreq-promotions-coupon-snapshots` (R-11); `…-upreq-fx-rate-snapshots` (R-07) |
| 04 usage-intake | `…-upreq-usage-feed-v1` (R-01); `…-upreq-usage-type-declaration-read` |
| 05 attribution-counters | `…-upreq-subscriptions-attribution-segments` (R-25); `resource` scope needs scope proofs (`aggregation_scope` exists in pricing, R-23 closed) |
| 06 fact-scheduling | `…-upreq-subscriptions-versioned-facts` (R-03); `…-upreq-subscriptions-usage-fact` (R-20); `…-upreq-subscriptions-recovery-reads`; `…-upreq-delay-only-acceptance` or coverage (R-21); pins, plan revision, quantity and `BillingTerms` on facts (SUB-D-29) |
| 07 child-evaluation | derived declarations (R-31); `…-upreq-subscriptions-scope-proofs`; `…-upreq-coverage-declarations`, `…-upreq-resource-history` (R-21); `…-upreq-accepted-binding-naming` (R-29); one-time rating `…-upreq-subscriptions-one-time-rating` (R-19) |
| 08 corrections-rerate | none beyond 07 and 09 |
| 09 rollup-delivery | `…-upreq-billing-delivery-consumer` (R-05), `…-upreq-billing-invoice-line-contract` (R-32), `…-upreq-billing-period-ownership` (R-04); `…-upreq-billing-ledger-item-granularity` (R-26) |
| 10 order-evaluation | `…-upreq-orders-evaluation-dto` (R-24) |
| 11 operations | `…-upreq-usage-reconciliation-access`; `…-upreq-tenant-deletion-signal` |
| no feature blocked | `…-upreq-pricing-meter-binding` (closed: a usage SKU sells a derived usage type, products P-D-259; R-09), `…-upreq-ledger-credit-note-wording` (documents only), `…-upreq-fixtures-band-scale` (closed: the corpus was deleted, UPSTREAM_REQS H-2) |

A double permits local development; it never satisfies a production dependency, and no feature marks
itself implemented because its document exists.

## 4. Contract address index

The sixteen contract namespaces `01`–`16` keep the section addresses of the former `design/NN-*.md`
slice documents, which decisions, ADRs, reviews and earlier revisions cite (for example "slice 14
§4.5", "`design/11` §4.2"). The table resolves each address to its current canonical section. A bare
section reference inside a detailed contract uses that contract's namespace; an explicit
cross-contract citation names its namespace (`14 §4.5`). These addresses are not file paths.

| Namespace | Former slice document | Title |
|---|---|---|
| 01 | `design/01-foundation.md` | Evaluation Foundation (pure-function core) |
| 02 | `design/02-selection-eligibility.md` | Base Selection & Eligibility |
| 03 | `design/03-metering-models.md` | Metering & Pricing Models |
| 04 | `design/04-overlays-precedence.md` | Overlays & Precedence |
| 05 | `design/05-commitments-reservations.md` | Commitments & Reservations |
| 06 | `design/06-coupons.md` | Coupons |
| 07 | `design/07-currency-fx.md` | Multi-Currency & FX |
| 08 | `design/08-retroactivity-corrections.md` | Retroactivity & Corrections |
| 09 | `design/09-period-plan-change.md` | Period & Plan-Change Obligations |
| 10 | `design/10-governance-asc606.md` | Governance & ASC 606 |
| 11 | `design/11-consumer-contracts.md` | Integration Contracts |
| 12 | `design/12-usage-ingestion-normalization.md` | Usage Ingestion & Normalization |
| 13 | `design/13-q-store-attribution.md` | Windowed Counters, Attribution Projection & Scope Evidence |
| 14 | `design/14-unit-synthesis-period-tick.md` | Facts, Child Windows, Scheduler & the Rater |
| 15 | `design/15-rated-output-balance-effects.md` | Window Results, Parent Roll-Up & Snapshots |
| 16 | `design/16-billing-handoff-operations.md` | Billing Delivery, Recovery & Operations |

| Contract address | Canonical section |
|------------------|-------------------|
| 01 §1.1 | [Architectural Vision](DESIGN.md#contract-01-1-1) |
| 01 §1.2 | [Architecture Drivers](DESIGN.md#contract-01-1-2) |
| 01 §1.3 | [Architecture Layers](DESIGN.md#contract-01-1-3) |
| 01 §2.1 | [Design Principles](DESIGN.md#contract-01-2-1) |
| 01 §2.2 | [Constraints](DESIGN.md#contract-01-2-2) |
| 01 §3.1 | [Domain Model](DESIGN.md#contract-01-3-1) |
| 01 §3.2 | [Component Model](DESIGN.md#contract-01-3-2) |
| 01 §3.3 | [API Contracts](DESIGN.md#contract-01-3-3) |
| 01 §3.4 | [Internal Dependencies](DESIGN.md#contract-01-3-4) |
| 01 §3.5 | [External Dependencies](DESIGN.md#contract-01-3-5) |
| 01 §3.6 | [Interactions and Sequences](features/02-evaluation-core.md#contract-01-3-6) |
| 01 §3.7 | [Database Schemas and Tables](DESIGN.md#contract-01-3-7) |
| 01 §3.8 | [Deployment Topology](DESIGN.md#contract-01-3-8) |
| 01 §4.1 | [Adopted Canonical Scope Key (normative)](DESIGN.md#contract-01-4-1) |
| 01 §4.2 | [Determinism and Idempotency Contract (normative)](DESIGN.md#contract-01-4-2) |
| 01 §4.3 | [Snapshot Composition (normative)](DESIGN.md#contract-01-4-3) |
| 01 §4.4 | [Emission Guards and Error Taxonomy (normative)](DESIGN.md#contract-01-4-4) |
| 01 §5 | [Traceability](DESIGN.md#contract-01-5) |
| 02 §1.1 | [Architectural Vision](DESIGN.md#contract-02-1-1) |
| 02 §1.2 | [Architecture Drivers](DESIGN.md#contract-02-1-2) |
| 02 §1.3 | [Architecture Layers](DESIGN.md#contract-02-1-3) |
| 02 §2.1 | [Design Principles](DESIGN.md#contract-02-2-1) |
| 02 §2.2 | [Constraints](DESIGN.md#contract-02-2-2) |
| 02 §3.1 | [Domain Model](DESIGN.md#contract-02-3-1) |
| 02 §3.2 | [Component Model](DESIGN.md#contract-02-3-2) |
| 02 §3.3 | [API Contracts](DESIGN.md#contract-02-3-3) |
| 02 §3.4 | [Internal Dependencies](DESIGN.md#contract-02-3-4) |
| 02 §3.5 | [External Dependencies](DESIGN.md#contract-02-3-5) |
| 02 §3.6 | [Interactions and Sequences](features/02-evaluation-core.md#contract-02-3-6) |
| 02 §3.7 | [Database Schemas and Tables](DESIGN.md#contract-02-3-7) |
| 02 §3.8 | [Deployment Topology](DESIGN.md#contract-02-3-8) |
| 02 §4.1 | [Selection Algorithm (normative)](DESIGN.md#contract-02-4-1) |
| 02 §4.2 | [Eligibility and Cohorts — Superseded (normative)](DESIGN.md#contract-02-4-2) |
| 02 §4.3 | [Phase Semantics (normative)](DESIGN.md#contract-02-4-3) |
| 02 §4.4 | [Selection Failure Taxonomy (normative)](DESIGN.md#contract-02-4-4) |
| 02 §5 | [Traceability](DESIGN.md#contract-02-5) |
| 03 §1.1 | [Architectural Vision](DESIGN.md#contract-03-1-1) |
| 03 §1.2 | [Architecture Drivers](DESIGN.md#contract-03-1-2) |
| 03 §1.3 | [Architecture Layers](DESIGN.md#contract-03-1-3) |
| 03 §2.1 | [Design Principles](DESIGN.md#contract-03-2-1) |
| 03 §2.2 | [Constraints](DESIGN.md#contract-03-2-2) |
| 03 §3.1 | [Domain Model](DESIGN.md#contract-03-3-1) |
| 03 §3.2 | [Component Model](DESIGN.md#contract-03-3-2) |
| 03 §3.3 | [API Contracts](DESIGN.md#contract-03-3-3) |
| 03 §3.4 | [Internal Dependencies](DESIGN.md#contract-03-3-4) |
| 03 §3.5 | [External Dependencies](DESIGN.md#contract-03-3-5) |
| 03 §3.6 | [Interactions and Sequences](features/02-evaluation-core.md#contract-03-3-6) |
| 03 §3.7 | [Database Schemas and Tables](DESIGN.md#contract-03-3-7) |
| 03 §3.8 | [Deployment Topology](DESIGN.md#contract-03-3-8) |
| 03 §4.1 | [Model Formulas (normative)](DESIGN.md#contract-03-4-1) |
| 03 §4.2 | [Meter Mapping and Dimensional Lines (normative)](DESIGN.md#contract-03-4-2) |
| 03 §4.3 | [Tier Aggregation Window, Slices and Band Continuity (normative)](DESIGN.md#contract-03-4-3) |
| 03 §4.4 | [Granularity Round-Up (normative)](DESIGN.md#contract-03-4-4) |
| 03 §5 | [Traceability](DESIGN.md#contract-03-5) |
| 04 §1.1 | [Architectural Vision](DESIGN.md#contract-04-1-1) |
| 04 §1.2 | [Architecture Drivers](DESIGN.md#contract-04-1-2) |
| 04 §1.3 | [Architecture Layers](DESIGN.md#contract-04-1-3) |
| 04 §2.1 | [Design Principles](DESIGN.md#contract-04-2-1) |
| 04 §2.2 | [Constraints](DESIGN.md#contract-04-2-2) |
| 04 §3.1 | [Domain Model](DESIGN.md#contract-04-3-1) |
| 04 §3.2 | [Component Model](DESIGN.md#contract-04-3-2) |
| 04 §3.3 | [API Contracts](DESIGN.md#contract-04-3-3) |
| 04 §3.4 | [Internal Dependencies](DESIGN.md#contract-04-3-4) |
| 04 §3.5 | [External Dependencies](DESIGN.md#contract-04-3-5) |
| 04 §3.6 | [Interactions and Sequences](features/03-price-adjustments.md#contract-04-3-6) |
| 04 §3.7 | [Database Schemas and Tables](DESIGN.md#contract-04-3-7) |
| 04 §3.8 | [Deployment Topology](DESIGN.md#contract-04-3-8) |
| 04 §4.1 | [Scope → Tenant-Axis Mapping (normative)](DESIGN.md#contract-04-4-1) |
| 04 §4.2 | [Stacking and the Total Order (normative)](DESIGN.md#contract-04-4-2) |
| 04 §4.3 | [Contract Overlay Precedence (normative)](DESIGN.md#contract-04-4-3) |
| 04 §4.4 | [Bounded Composition — Anti-Drift Cap (normative)](DESIGN.md#contract-04-4-4) |
| 04 §5 | [Traceability](DESIGN.md#contract-04-5) |
| 05 §1.1 | [Architectural Vision](DESIGN.md#contract-05-1-1) |
| 05 §1.2 | [Architecture Drivers](DESIGN.md#contract-05-1-2) |
| 05 §1.3 | [Architecture Layers](DESIGN.md#contract-05-1-3) |
| 05 §2.1 | [Design Principles](DESIGN.md#contract-05-2-1) |
| 05 §2.2 | [Constraints](DESIGN.md#contract-05-2-2) |
| 05 §3.1 | [Domain Model](DESIGN.md#contract-05-3-1) |
| 05 §3.2 | [Component Model](DESIGN.md#contract-05-3-2) |
| 05 §3.3 | [API Contracts](DESIGN.md#contract-05-3-3) |
| 05 §3.4 | [Internal Dependencies](DESIGN.md#contract-05-3-4) |
| 05 §3.5 | [External Dependencies](DESIGN.md#contract-05-3-5) |
| 05 §3.6 | [Interactions and Sequences](features/03-price-adjustments.md#contract-05-3-6) |
| 05 §3.7 | [Database Schemas and Tables](DESIGN.md#contract-05-3-7) |
| 05 §3.8 | [Deployment Topology](DESIGN.md#contract-05-3-8) |
| 05 §4.1 | [Commitment-Pool Waterfall (normative)](DESIGN.md#contract-05-4-1) |
| 05 §4.2 | [Reservation Flavors and Pool Precedence (normative)](DESIGN.md#contract-05-4-2) |
| 05 §4.3 | [Reserved-Rate Two-Source Rule (normative)](DESIGN.md#contract-05-4-3) |
| 05 §4.4 | [Commitment Pool vs Prepaid Credit Grant (normative)](DESIGN.md#contract-05-4-4) |
| 05 §4.5 | [Obligations, Reversals, and Period Boundaries (normative)](DESIGN.md#contract-05-4-5) |
| 05 §5 | [Traceability](DESIGN.md#contract-05-5) |
| 06 §1.1 | [Architectural Vision](DESIGN.md#contract-06-1-1) |
| 06 §1.2 | [Architecture Drivers](DESIGN.md#contract-06-1-2) |
| 06 §1.3 | [Architecture Layers](DESIGN.md#contract-06-1-3) |
| 06 §2.1 | [Design Principles](DESIGN.md#contract-06-2-1) |
| 06 §2.2 | [Constraints](DESIGN.md#contract-06-2-2) |
| 06 §3.1 | [Domain Model](DESIGN.md#contract-06-3-1) |
| 06 §3.2 | [Component Model](DESIGN.md#contract-06-3-2) |
| 06 §3.3 | [API Contracts](DESIGN.md#contract-06-3-3) |
| 06 §3.4 | [Internal Dependencies](DESIGN.md#contract-06-3-4) |
| 06 §3.5 | [External Dependencies](DESIGN.md#contract-06-3-5) |
| 06 §3.6 | [Interactions and Sequences](features/03-price-adjustments.md#contract-06-3-6) |
| 06 §3.7 | [Database Schemas and Tables](DESIGN.md#contract-06-3-7) |
| 06 §3.8 | [Deployment Topology](DESIGN.md#contract-06-3-8) |
| 06 §4.1 | [Placement and the FX Split (normative)](DESIGN.md#contract-06-4-1) |
| 06 §4.2 | [Stacking Policies (normative)](DESIGN.md#contract-06-4-2) |
| 06 §4.3 | [applyScope Attachment and Hybrid Split-Back (normative)](DESIGN.md#contract-06-4-3) |
| 06 §4.4 | [Frozen Coupon Snapshot and Fail-Closed Rules (normative)](DESIGN.md#contract-06-4-4) |
| 06 §4.5 | [Snapshot Segment and Discount Lineage (normative)](DESIGN.md#contract-06-4-5) |
| 06 §5 | [Traceability](DESIGN.md#contract-06-5) |
| 07 §1.1 | [Architectural Vision](DESIGN.md#contract-07-1-1) |
| 07 §1.2 | [Architecture Drivers](DESIGN.md#contract-07-1-2) |
| 07 §1.3 | [Architecture Layers](DESIGN.md#contract-07-1-3) |
| 07 §2.1 | [Design Principles](DESIGN.md#contract-07-2-1) |
| 07 §2.2 | [Constraints](DESIGN.md#contract-07-2-2) |
| 07 §3.1 | [Domain Model](DESIGN.md#contract-07-3-1) |
| 07 §3.2 | [Component Model](DESIGN.md#contract-07-3-2) |
| 07 §3.3 | [API Contracts](DESIGN.md#contract-07-3-3) |
| 07 §3.4 | [Internal Dependencies](DESIGN.md#contract-07-3-4) |
| 07 §3.5 | [External Dependencies](DESIGN.md#contract-07-3-5) |
| 07 §3.6 | [Interactions and Sequences](features/03-price-adjustments.md#contract-07-3-6) |
| 07 §3.7 | [Database Schemas and Tables](DESIGN.md#contract-07-3-7) |
| 07 §3.8 | [Deployment Topology](DESIGN.md#contract-07-3-8) |
| 07 §4.1 | [Currency Role Separation (normative)](DESIGN.md#contract-07-4-1) |
| 07 §4.2 | [FX Policy Semantics (normative)](DESIGN.md#contract-07-4-2) |
| 07 §4.3 | [FX-Lock Snapshot Segment (normative)](DESIGN.md#contract-07-4-3) |
| 07 §4.4 | [Ordering and Precision at the FX Boundary (normative)](DESIGN.md#contract-07-4-4) |
| 07 §5 | [Traceability](DESIGN.md#contract-07-5) |
| 08 §1.1 | [Architectural Vision](DESIGN.md#contract-08-1-1) |
| 08 §1.2 | [Architecture Drivers](DESIGN.md#contract-08-1-2) |
| 08 §1.3 | [Architecture Layers](DESIGN.md#contract-08-1-3) |
| 08 §2.1 | [Design Principles](DESIGN.md#contract-08-2-1) |
| 08 §2.2 | [Constraints](DESIGN.md#contract-08-2-2) |
| 08 §3.1 | [Domain Model](DESIGN.md#contract-08-3-1) |
| 08 §3.2 | [Component Model](DESIGN.md#contract-08-3-2) |
| 08 §3.3 | [API Contracts](DESIGN.md#contract-08-3-3) |
| 08 §3.4 | [Internal Dependencies](DESIGN.md#contract-08-3-4) |
| 08 §3.5 | [External Dependencies](DESIGN.md#contract-08-3-5) |
| 08 §3.6 | [Interactions and Sequences](features/08-corrections-rerate.md#contract-08-3-6) |
| 08 §3.7 | [Database Schemas and Tables](DESIGN.md#contract-08-3-7) |
| 08 §3.8 | [Deployment Topology](DESIGN.md#contract-08-3-8) |
| 08 §4.1 | [Replay, Re-evaluation and the Binding of Record (normative)](DESIGN.md#contract-08-4-1) |
| 08 §4.2 | [Correction Keys and Idempotency (normative)](DESIGN.md#contract-08-4-2) |
| 08 §4.3 | [Posted Periods (normative)](DESIGN.md#contract-08-4-3) |
| 08 §4.4 | [Reversal Math and Emission Guards (normative)](DESIGN.md#contract-08-4-4) |
| 08 §4.5 | [Acceptance Vectors](features/08-corrections-rerate.md#contract-08-4-5) |
| 08 §5 | [Traceability](DESIGN.md#contract-08-5) |
| 09 §1.1 | [Architectural Vision](DESIGN.md#contract-09-1-1) |
| 09 §1.2 | [Architecture Drivers](DESIGN.md#contract-09-1-2) |
| 09 §1.3 | [Architecture Layers](DESIGN.md#contract-09-1-3) |
| 09 §2.1 | [Design Principles](DESIGN.md#contract-09-2-1) |
| 09 §2.2 | [Constraints](DESIGN.md#contract-09-2-2) |
| 09 §3.1 | [Domain Model](DESIGN.md#contract-09-3-1) |
| 09 §3.2 | [Component Model](DESIGN.md#contract-09-3-2) |
| 09 §3.3 | [API Contracts](DESIGN.md#contract-09-3-3) |
| 09 §3.4 | [Internal Dependencies](DESIGN.md#contract-09-3-4) |
| 09 §3.5 | [External Dependencies](DESIGN.md#contract-09-3-5) |
| 09 §3.6 | [Interactions and Sequences](features/02-evaluation-core.md#contract-09-3-6) |
| 09 §3.7 | [Database Schemas and Tables](DESIGN.md#contract-09-3-7) |
| 09 §3.8 | [Deployment Topology](DESIGN.md#contract-09-3-8) |
| 09 §4.1 | [Proration and Billing Terms (normative)](DESIGN.md#contract-09-4-1) |
| 09 §4.2 | [Minimum-Fee Floor (normative)](DESIGN.md#contract-09-4-2) |
| 09 §4.3 | [Sub-Window Split Semantics (normative)](DESIGN.md#contract-09-4-3) |
| 09 §5 | [Traceability](DESIGN.md#contract-09-5) |
| 10 §1.1 | [Architectural Vision](DESIGN.md#contract-10-1-1) |
| 10 §1.2 | [Architecture Drivers](DESIGN.md#contract-10-1-2) |
| 10 §1.3 | [Architecture Layers](DESIGN.md#contract-10-1-3) |
| 10 §2.1 | [Design Principles](DESIGN.md#contract-10-2-1) |
| 10 §2.2 | [Constraints](DESIGN.md#contract-10-2-2) |
| 10 §3.1 | [Domain Model](DESIGN.md#contract-10-3-1) |
| 10 §3.2 | [Component Model](DESIGN.md#contract-10-3-2) |
| 10 §3.3 | [API Contracts](DESIGN.md#contract-10-3-3) |
| 10 §3.4 | [Internal Dependencies](DESIGN.md#contract-10-3-4) |
| 10 §3.5 | [External Dependencies](DESIGN.md#contract-10-3-5) |
| 10 §3.6 | [Interactions and Sequences](features/02-evaluation-core.md#contract-10-3-6) |
| 10 §3.7 | [Database Schemas and Tables](DESIGN.md#contract-10-3-7) |
| 10 §3.8 | [Deployment Topology](DESIGN.md#contract-10-3-8) |
| 10 §4.1 | [Single Governance Engine (normative)](DESIGN.md#contract-10-4-1) |
| 10 §4.2 | [Rating-Time Checks (normative)](DESIGN.md#contract-10-4-2) |
| 10 §4.3 | [Ledger Separation Rationale (normative)](DESIGN.md#contract-10-4-3) |
| 10 §4.4 | [ASC 606 Reference Emission (normative)](DESIGN.md#contract-10-4-4) |
| 10 §4.5 | [Bundle Rev-Share Pass-Through (normative)](DESIGN.md#contract-10-4-5) |
| 10 §4.6 | [AuthZ Resource and Action Catalog (normative)](DESIGN.md#contract-10-4-6) |
| 10 §5 | [Traceability](DESIGN.md#contract-10-5) |
| 11 §1.1 | [Architectural Vision](DESIGN.md#contract-11-1-1) |
| 11 §1.2 | [Architecture Drivers](DESIGN.md#contract-11-1-2) |
| 11 §1.3 | [Architecture Layers](DESIGN.md#contract-11-1-3) |
| 11 §2.1 | [Design Principles](DESIGN.md#contract-11-2-1) |
| 11 §2.2 | [Constraints](DESIGN.md#contract-11-2-2) |
| 11 §3.1 | [Domain Model](DESIGN.md#contract-11-3-1) |
| 11 §3.2 | [Component Model](DESIGN.md#contract-11-3-2) |
| 11 §3.3 | [API Contracts](DESIGN.md#contract-11-3-3) |
| 11 §3.4 | [Internal Dependencies](DESIGN.md#contract-11-3-4) |
| 11 §3.5 | [External Dependencies](DESIGN.md#contract-11-3-5) |
| 11 §3.6 | [Interactions and Sequences](features/07-child-evaluation.md#contract-11-3-6) |
| 11 §3.7 | [Database Schemas and Tables](DESIGN.md#contract-11-3-7) |
| 11 §3.8 | [Deployment Topology](DESIGN.md#contract-11-3-8) |
| 11 §4.1 | [Rating Handoff Contract (normative)](DESIGN.md#contract-11-4-1) |
| 11 §4.2 | [Pricing Read-Model Input Contract (normative)](DESIGN.md#contract-11-4-2) |
| 11 §4.3 | [Subscriptions Input Contract (normative)](DESIGN.md#contract-11-4-3) |
| 11 §4.4 | [Finance FX Input Contract (normative)](DESIGN.md#contract-11-4-4) |
| 11 §4.5 | [Promotions Coupon Snapshot Contract (normative)](DESIGN.md#contract-11-4-5) |
| 11 §4.6 | [Billing Delivery and Obligation Contract (normative)](DESIGN.md#contract-11-4-6) |
| 11 §4.7 | [Rating Snapshot Reference (`pricing_snapshot_ref`) (normative)](DESIGN.md#contract-11-4-7) |
| 11 §4.8 | [Canonical Naming (normative)](DESIGN.md#contract-11-4-8) |
| 11 §4.9 | [Contracts & Agreements Input Contract (normative)](DESIGN.md#contract-11-4-9) |
| 11 §4.10 | [Order Evaluation Contract (normative)](DESIGN.md#contract-11-4-10) |
| 11 §4.11 | [Inbox and Recovery (normative)](DESIGN.md#contract-11-4-11) |
| 11 §4.12 | [Provider Test Doubles (normative)](features/01-foundation.md#contract-11-4-12) |
| 11 §4.13 | [Rating-provided Contract Governance (normative)](DESIGN.md#contract-11-4-13) |
| 11 §5 | [Traceability](DESIGN.md#contract-11-5) |
| 12 §1.1 | [Architectural Vision](DESIGN.md#contract-12-1-1) |
| 12 §1.2 | [Architecture Drivers](DESIGN.md#contract-12-1-2) |
| 12 §1.3 | [Architecture Layers](DESIGN.md#contract-12-1-3) |
| 12 §2.1 | [Design Principles](DESIGN.md#contract-12-2-1) |
| 12 §2.2 | [Constraints](DESIGN.md#contract-12-2-2) |
| 12 §3.1 | [Domain Model](DESIGN.md#contract-12-3-1) |
| 12 §3.2 | [Component Model](DESIGN.md#contract-12-3-2) |
| 12 §3.3 | [API Contracts](DESIGN.md#contract-12-3-3) |
| 12 §3.4 | [Internal Dependencies](DESIGN.md#contract-12-3-4) |
| 12 §3.5 | [External Dependencies](DESIGN.md#contract-12-3-5) |
| 12 §3.6 | [Interactions and Sequences](features/04-usage-intake.md#contract-12-3-6) |
| 12 §3.7 | [Database Schemas and Tables](DESIGN.md#contract-12-3-7) |
| 12 §3.8 | [Deployment Topology](DESIGN.md#contract-12-3-8) |
| 12 §4.1 | [Collector Entry → Rating Usage Mapping (normative)](DESIGN.md#contract-12-4-1) |
| 12 §4.2 | [Attribution (normative)](DESIGN.md#contract-12-4-2) |
| 12 §4.3 | [Usage Dedup (normative)](DESIGN.md#contract-12-4-3) |
| 12 §4.4 | [Invalidations, Replacements and Negative Quantities (normative)](features/04-usage-intake.md#contract-12-4-4) |
| 12 §4.5 | [Exceptions (normative)](features/04-usage-intake.md#contract-12-4-5) |
| 12 §4.6 | [Feed Checkpoint, Retention and Backpressure (normative)](features/04-usage-intake.md#contract-12-4-6) |
| 12 §4.7 | [Usage Type Declarations (normative)](DESIGN.md#contract-12-4-7) |
| 12 §5 | [Traceability](DESIGN.md#contract-12-5) |
| 13 §1.1 | [Architectural Vision](DESIGN.md#contract-13-1-1) |
| 13 §1.2 | [Architecture Drivers](DESIGN.md#contract-13-1-2) |
| 13 §1.3 | [Architecture Layers](DESIGN.md#contract-13-1-3) |
| 13 §2.1 | [Design Principles](DESIGN.md#contract-13-2-1) |
| 13 §2.2 | [Constraints](DESIGN.md#contract-13-2-2) |
| 13 §3.1 | [Domain Model](DESIGN.md#contract-13-3-1) |
| 13 §3.2 | [Component Model](DESIGN.md#contract-13-3-2) |
| 13 §3.3 | [API Contracts](DESIGN.md#contract-13-3-3) |
| 13 §3.4 | [Internal Dependencies](DESIGN.md#contract-13-3-4) |
| 13 §3.5 | [External Dependencies](DESIGN.md#contract-13-3-5) |
| 13 §3.6 | [Interactions and Sequences](features/05-attribution-counters.md#contract-13-3-6) |
| 13 §3.7 | [Database Schemas and Tables](DESIGN.md#contract-13-3-7) |
| 13 §3.8 | [Deployment Topology](DESIGN.md#contract-13-3-8) |
| 13 §4.1 | [Counter Key and Window Resolution (normative)](DESIGN.md#contract-13-4-1) |
| 13 §4.2 | [Row Serialization and q_version (normative)](DESIGN.md#contract-13-4-2) |
| 13 §4.3 | [Slice Layout (normative)](DESIGN.md#contract-13-4-3) |
| 13 §4.4 | [Re-Materialization (normative)](features/05-attribution-counters.md#contract-13-4-4) |
| 13 §4.5 | [Composite Inputs (normative)](DESIGN.md#contract-13-4-5) |
| 13 §4.6 | [Attribution Projection (normative)](features/05-attribution-counters.md#contract-13-4-6) |
| 13 §4.7 | [Scope and Coverage Evidence (normative)](DESIGN.md#contract-13-4-7) |
| 13 §5 | [Traceability](DESIGN.md#contract-13-5) |
| 14 §1.1 | [Architectural Vision](DESIGN.md#contract-14-1-1) |
| 14 §1.2 | [Architecture Drivers](DESIGN.md#contract-14-1-2) |
| 14 §1.3 | [Architecture Layers](DESIGN.md#contract-14-1-3) |
| 14 §2.1 | [Design Principles](DESIGN.md#contract-14-2-1) |
| 14 §2.2 | [Constraints](DESIGN.md#contract-14-2-2) |
| 14 §3.1 | [Domain Model](DESIGN.md#contract-14-3-1) |
| 14 §3.2 | [Component Model](DESIGN.md#contract-14-3-2) |
| 14 §3.3 | [API Contracts](DESIGN.md#contract-14-3-3) |
| 14 §3.4 | [Internal Dependencies](DESIGN.md#contract-14-3-4) |
| 14 §3.5 | [External Dependencies](DESIGN.md#contract-14-3-5) |
| 14 §3.6 (evaluate a child) | [Interactions and Sequences](features/07-child-evaluation.md#contract-14-3-6) |
| 14 §3.7 | [Database Schemas and Tables](DESIGN.md#contract-14-3-7) |
| 14 §3.8 | [Deployment Topology](DESIGN.md#contract-14-3-8) |
| 14 §4.1 | [Child Kinds (normative)](DESIGN.md#contract-14-4-1) |
| 14 §4.2 | [Commercial Facts (normative)](DESIGN.md#contract-14-4-2) |
| 14 §4.3 | [Input Assembly (normative)](features/07-child-evaluation.md#contract-14-4-3) |
| 14 §4.4 | [Work Queue, Scheduler and Coalescing (normative)](features/06-fact-scheduling.md#contract-14-4-4) |
| 14 §4.5 | [Finalization Gate (normative)](DESIGN.md#contract-14-4-5) |
| 14 §4.6 | [Commitment Balances (normative)](DESIGN.md#contract-14-4-6) |
| 14 §4.7 | [Acceptance Vectors](features/06-fact-scheduling.md#contract-14-4-7) |
| 14 §5 | [Traceability](DESIGN.md#contract-14-5) |
| 15 §1.1 | [Architectural Vision](DESIGN.md#contract-15-1-1) |
| 15 §1.2 | [Architecture Drivers](DESIGN.md#contract-15-1-2) |
| 15 §1.3 | [Architecture Layers](DESIGN.md#contract-15-1-3) |
| 15 §2.1 | [Design Principles](DESIGN.md#contract-15-2-1) |
| 15 §2.2 | [Constraints](DESIGN.md#contract-15-2-2) |
| 15 §3.1 | [Domain Model](DESIGN.md#contract-15-3-1) |
| 15 §3.2 | [Component Model](DESIGN.md#contract-15-3-2) |
| 15 §3.3 | [API Contracts](DESIGN.md#contract-15-3-3) |
| 15 §3.4 | [Internal Dependencies](DESIGN.md#contract-15-3-4) |
| 15 §3.5 | [External Dependencies](DESIGN.md#contract-15-3-5) |
| 15 §3.6 (persist a final outcome) | [Interactions and Sequences](features/07-child-evaluation.md#contract-15-3-6) |
| 15 §3.7 | [Database Schemas and Tables](DESIGN.md#contract-15-3-7) |
| 15 §3.8 | [Deployment Topology](DESIGN.md#contract-15-3-8) |
| 15 §4.1 | [Outcome → Results (normative)](DESIGN.md#contract-15-4-1) |
| 15 §4.2 | [Result Revisions (normative)](DESIGN.md#contract-15-4-2) |
| 15 §4.3 | [Line Identities and Idempotency (normative)](DESIGN.md#contract-15-4-3) |
| 15 §4.4 | [Exact Amounts (normative)](DESIGN.md#contract-15-4-4) |
| 15 §4.5 | [CommitmentBalanceEffect (normative)](DESIGN.md#contract-15-4-5) |
| 15 §4.6 | [Acceptance Vectors](features/09-rollup-delivery.md#contract-15-4-6) |
| 15 §5 | [Traceability](DESIGN.md#contract-15-5) |
| 16 §1.1 | [Architectural Vision](DESIGN.md#contract-16-1-1) |
| 16 §1.2 | [Architecture Drivers](DESIGN.md#contract-16-1-2) |
| 16 §1.3 | [Architecture Layers](DESIGN.md#contract-16-1-3) |
| 16 §2.1 | [Design Principles](DESIGN.md#contract-16-2-1) |
| 16 §2.2 | [Constraints](DESIGN.md#contract-16-2-2) |
| 16 §3.1 | [Domain Model](DESIGN.md#contract-16-3-1) |
| 16 §3.2 | [Component Model](DESIGN.md#contract-16-3-2) |
| 16 §3.3 | [API Contracts](DESIGN.md#contract-16-3-3) |
| 16 §3.4 | [Internal Dependencies](DESIGN.md#contract-16-3-4) |
| 16 §3.5 | [External Dependencies](DESIGN.md#contract-16-3-5) |
| 16 §3.6 | [Interactions and Sequences](features/09-rollup-delivery.md#contract-16-3-6) |
| 16 §3.7 | [Database Schemas and Tables](DESIGN.md#contract-16-3-7) |
| 16 §3.8 | [Deployment Topology](DESIGN.md#contract-16-3-8) |
| 16 §4.1 | [Billing Delivery Contract (normative)](DESIGN.md#contract-16-4-1) |
| 16 §4.2 | [Period State Is Billing's (normative)](DESIGN.md#contract-16-4-2) |
| 16 §4.3 | [Operational Topology (normative)](DESIGN.md#contract-16-4-3) |
| 16 §4.4 | [Backpressure, Replay, Cold Start (normative)](features/11-operations.md#contract-16-4-4) |
| 16 §4.5 | [NFR Verification (normative)](features/11-operations.md#contract-16-4-5) |
| 16 §5 | [Traceability](DESIGN.md#contract-16-5) |
| 14 §3.6 (fact intake, scheduler, wake-ups) | [Interactions and Sequences](features/06-fact-scheduling.md#contract-14-3-6-f06) |
| 15 §3.6 (revision worked example) | [Interactions and Sequences](features/09-rollup-delivery.md#contract-15-3-6-f09) |

### 4.1 Identifier migration

The former slice documents were outside CFS validation; in DESIGN and the features they are
validated artifacts, whose ID kinds are fixed by the kit. IDs whose kind the kit does not allow were
given an allowed kind with the rest of the ID unchanged: `normative` became `design` (in DESIGN) or
`algo` (in a feature), `domain` became `entity`, `datastore` became `db`, `deployment` became
`topology`. No other ID changed. No document outside Rating cited any of the old IDs.

| Former ID (suffix after the common Rating ID prefix) | Current ID |
|---|---|
| datastore-billing-handoff-bhf | `cpt-cf-bss-rating-db-billing-handoff-bhf` |
| datastore-ingestion-ing | `cpt-cf-bss-rating-db-ingestion-ing` |
| datastore-none-cc | `cpt-cf-bss-rating-db-none-cc` |
| datastore-none-cmt | `cpt-cf-bss-rating-db-none-cmt` |
| datastore-none-cpn | `cpt-cf-bss-rating-db-none-cpn` |
| datastore-none-fnd | `cpt-cf-bss-rating-db-none-fnd` |
| datastore-none-fx | `cpt-cf-bss-rating-db-none-fx` |
| datastore-none-gov | `cpt-cf-bss-rating-db-none-gov` |
| datastore-none-mm | `cpt-cf-bss-rating-db-none-mm` |
| datastore-none-ovl | `cpt-cf-bss-rating-db-none-ovl` |
| datastore-none-ppc | `cpt-cf-bss-rating-db-none-ppc` |
| datastore-none-rtr | `cpt-cf-bss-rating-db-none-rtr` |
| datastore-none-sel | `cpt-cf-bss-rating-db-none-sel` |
| datastore-q-qst | `cpt-cf-bss-rating-db-q-qst` |
| datastore-rated-output-rob | `cpt-cf-bss-rating-db-rated-output-rob` |
| datastore-synthesis-syn | `cpt-cf-bss-rating-db-synthesis-syn` |
| datastore-tiering-ing | `cpt-cf-bss-rating-db-tiering-ing` |
| deployment-bhf | `cpt-cf-bss-rating-topology-bhf` |
| deployment-cc | `cpt-cf-bss-rating-topology-cc` |
| deployment-cmt | `cpt-cf-bss-rating-topology-cmt` |
| deployment-cpn | `cpt-cf-bss-rating-topology-cpn` |
| deployment-fnd | `cpt-cf-bss-rating-topology-fnd` |
| deployment-fx | `cpt-cf-bss-rating-topology-fx` |
| deployment-gov | `cpt-cf-bss-rating-topology-gov` |
| deployment-ing | `cpt-cf-bss-rating-topology-ing` |
| deployment-mm | `cpt-cf-bss-rating-topology-mm` |
| deployment-ovl | `cpt-cf-bss-rating-topology-ovl` |
| deployment-ppc | `cpt-cf-bss-rating-topology-ppc` |
| deployment-qst | `cpt-cf-bss-rating-topology-qst` |
| deployment-rob | `cpt-cf-bss-rating-topology-rob` |
| deployment-rtr | `cpt-cf-bss-rating-topology-rtr` |
| deployment-sel | `cpt-cf-bss-rating-topology-sel` |
| deployment-syn | `cpt-cf-bss-rating-topology-syn` |
| domain-model-bhf | `cpt-cf-bss-rating-entity-model-bhf` |
| domain-model-cc | `cpt-cf-bss-rating-entity-model-cc` |
| domain-model-cmt | `cpt-cf-bss-rating-entity-model-cmt` |
| domain-model-cpn | `cpt-cf-bss-rating-entity-model-cpn` |
| domain-model-fnd | `cpt-cf-bss-rating-entity-model-fnd` |
| domain-model-fx | `cpt-cf-bss-rating-entity-model-fx` |
| domain-model-gov | `cpt-cf-bss-rating-entity-model-gov` |
| domain-model-ing | `cpt-cf-bss-rating-entity-model-ing` |
| domain-model-mm | `cpt-cf-bss-rating-entity-model-mm` |
| domain-model-ovl | `cpt-cf-bss-rating-entity-model-ovl` |
| domain-model-ppc | `cpt-cf-bss-rating-entity-model-ppc` |
| domain-model-qst | `cpt-cf-bss-rating-entity-model-qst` |
| domain-model-rob | `cpt-cf-bss-rating-entity-model-rob` |
| domain-model-rtr | `cpt-cf-bss-rating-entity-model-rtr` |
| domain-model-sel | `cpt-cf-bss-rating-entity-model-sel` |
| domain-model-syn | `cpt-cf-bss-rating-entity-model-syn` |
| normative-adopted-key-fnd | `cpt-cf-bss-rating-design-adopted-key-fnd` |
| normative-anti-drift-cap-ovl | `cpt-cf-bss-rating-design-anti-drift-cap-ovl` |
| normative-applyscope-cpn | `cpt-cf-bss-rating-design-applyscope-cpn` |
| normative-asc606-refs-gov | `cpt-cf-bss-rating-design-asc606-refs-gov` |
| normative-attribution-projection-qst | `cpt-cf-bss-rating-algo-attribution-projection-qst` |
| normative-authz-catalog-gov | `cpt-cf-bss-rating-design-authz-catalog-gov` |
| normative-backpressure-replay-bhf | `cpt-cf-bss-rating-algo-backpressure-replay-bhf` |
| normative-balance-effect-rob | `cpt-cf-bss-rating-design-balance-effect-rob` |
| normative-balance-freeze-syn | `cpt-cf-bss-rating-design-balance-freeze-syn` |
| normative-billing-delivery-bhf | `cpt-cf-bss-rating-design-billing-delivery-bhf` |
| normative-billing-periodstate-cc | `cpt-cf-bss-rating-design-billing-periodstate-cc` |
| normative-canonical-naming-cc | `cpt-cf-bss-rating-design-canonical-naming-cc` |
| normative-cascade-routing-syn | `cpt-cf-bss-rating-algo-cascade-routing-syn` |
| normative-composite-assembly-qst | `cpt-cf-bss-rating-design-composite-assembly-qst` |
| normative-context-assembly-syn | `cpt-cf-bss-rating-algo-context-assembly-syn` |
| normative-contract-governance-cc | `cpt-cf-bss-rating-design-contract-governance-cc` |
| normative-contract-overlay-ovl | `cpt-cf-bss-rating-design-contract-overlay-ovl` |
| normative-contracts-input-cc | `cpt-cf-bss-rating-design-contracts-input-cc` |
| normative-correction-intake-ing | `cpt-cf-bss-rating-algo-correction-intake-ing` |
| normative-correction-key-rtr | `cpt-cf-bss-rating-design-correction-key-rtr` |
| normative-counter-key-qst | `cpt-cf-bss-rating-design-counter-key-qst` |
| normative-currency-roles-fx | `cpt-cf-bss-rating-design-currency-roles-fx` |
| normative-delta-dedup-rob | `cpt-cf-bss-rating-design-delta-dedup-rob` |
| normative-determinism-fnd | `cpt-cf-bss-rating-design-determinism-fnd` |
| normative-dimensional-mapping-mm | `cpt-cf-bss-rating-design-dimensional-mapping-mm` |
| normative-eligibility-cohort-sel | `cpt-cf-bss-rating-design-eligibility-cohort-sel` |
| normative-emission-guards-fnd | `cpt-cf-bss-rating-design-emission-guards-fnd` |
| normative-error-taxonomy-fnd | `cpt-cf-bss-rating-design-error-taxonomy-fnd` |
| normative-fail-closed-snapshot-cpn | `cpt-cf-bss-rating-design-fail-closed-snapshot-cpn` |
| normative-failure-taxonomy-sel | `cpt-cf-bss-rating-design-failure-taxonomy-sel` |
| normative-feed-checkpoint-ing | `cpt-cf-bss-rating-algo-feed-checkpoint-ing` |
| normative-finalization-gate-syn | `cpt-cf-bss-rating-design-finalization-gate-syn` |
| normative-finance-fx-cc | `cpt-cf-bss-rating-design-finance-fx-cc` |
| normative-floor-cap-envelope-ppc | `cpt-cf-bss-rating-design-floor-cap-envelope-ppc` |
| normative-four-validators-gov | `cpt-cf-bss-rating-design-four-validators-gov` |
| normative-fx-lock-segment-fx | `cpt-cf-bss-rating-design-fx-lock-segment-fx` |
| normative-fx-ordering-fx | `cpt-cf-bss-rating-design-fx-ordering-fx` |
| normative-fx-policy-fx | `cpt-cf-bss-rating-design-fx-policy-fx` |
| normative-granularity-mm | `cpt-cf-bss-rating-design-granularity-mm` |
| normative-inbox-recovery-cc | `cpt-cf-bss-rating-design-inbox-recovery-cc` |
| normative-lanes-bhf | `cpt-cf-bss-rating-design-lanes-bhf` |
| normative-ledger-separation-gov | `cpt-cf-bss-rating-design-ledger-separation-gov` |
| normative-model-formulas-mm | `cpt-cf-bss-rating-design-model-formulas-mm` |
| normative-nfr-verification-bhf | `cpt-cf-bss-rating-algo-nfr-verification-bhf` |
| normative-normalization-ing | `cpt-cf-bss-rating-design-normalization-ing` |
| normative-obligations-boundary-cmt | `cpt-cf-bss-rating-design-obligations-boundary-cmt` |
| normative-order-evaluation-cc | `cpt-cf-bss-rating-design-order-evaluation-cc` |
| normative-outcome-mapping-rob | `cpt-cf-bss-rating-design-outcome-mapping-rob` |
| normative-period-tick-syn | `cpt-cf-bss-rating-design-period-tick-syn` |
| normative-periodstate-relay-bhf | `cpt-cf-bss-rating-design-periodstate-relay-bhf` |
| normative-periodstate-routing-rtr | `cpt-cf-bss-rating-design-periodstate-routing-rtr` |
| normative-phase-semantics-sel | `cpt-cf-bss-rating-design-phase-semantics-sel` |
| normative-placement-fx-split-cpn | `cpt-cf-bss-rating-design-placement-fx-split-cpn` |
| normative-pool-vs-grant-cmt | `cpt-cf-bss-rating-design-pool-vs-grant-cmt` |
| normative-pricing-readmodel-cc | `cpt-cf-bss-rating-design-pricing-readmodel-cc` |
| normative-promotions-coupons-cc | `cpt-cf-bss-rating-design-promotions-coupons-cc` |
| normative-proration-enums-ppc | `cpt-cf-bss-rating-design-proration-enums-ppc` |
| normative-provisional-fx-rob | `cpt-cf-bss-rating-design-provisional-fx-rob` |
| normative-quarantine-ing | `cpt-cf-bss-rating-algo-quarantine-ing` |
| normative-rated-store-rob | `cpt-cf-bss-rating-design-rated-store-rob` |
| normative-rating-handoff-cc | `cpt-cf-bss-rating-design-rating-handoff-cc` |
| normative-rematerialize-qst | `cpt-cf-bss-rating-algo-rematerialize-qst` |
| normative-reservation-flavors-cmt | `cpt-cf-bss-rating-design-reservation-flavors-cmt` |
| normative-reserved-rate-sourcing-cmt | `cpt-cf-bss-rating-design-reserved-rate-sourcing-cmt` |
| normative-reversal-guards-rtr | `cpt-cf-bss-rating-design-reversal-guards-rtr` |
| normative-revshare-passthrough-gov | `cpt-cf-bss-rating-design-revshare-passthrough-gov` |
| normative-scope-coverage-qst | `cpt-cf-bss-rating-design-scope-coverage-qst` |
| normative-scope-mapping-ovl | `cpt-cf-bss-rating-design-scope-mapping-ovl` |
| normative-segment-lineage-cpn | `cpt-cf-bss-rating-design-segment-lineage-cpn` |
| normative-selection-algorithm-sel | `cpt-cf-bss-rating-design-selection-algorithm-sel` |
| normative-session-merge-ing | `cpt-cf-bss-rating-design-session-merge-ing` |
| normative-single-engine-gov | `cpt-cf-bss-rating-design-single-engine-gov` |
| normative-single-writer-qst | `cpt-cf-bss-rating-design-single-writer-qst` |
| normative-slice-attribution-qst | `cpt-cf-bss-rating-design-slice-attribution-qst` |
| normative-snapshot-composition-fnd | `cpt-cf-bss-rating-design-snapshot-composition-fnd` |
| normative-snapshot-replay-rtr | `cpt-cf-bss-rating-design-snapshot-replay-rtr` |
| normative-snapshot-segments-cc | `cpt-cf-bss-rating-design-snapshot-segments-cc` |
| normative-split-semantics-ppc | `cpt-cf-bss-rating-design-split-semantics-ppc` |
| normative-stacking-cpn | `cpt-cf-bss-rating-design-stacking-cpn` |
| normative-stacking-order-ovl | `cpt-cf-bss-rating-design-stacking-order-ovl` |
| normative-subscriptions-input-cc | `cpt-cf-bss-rating-design-subscriptions-input-cc` |
| normative-test-doubles-cc | `cpt-cf-bss-rating-algo-test-doubles-cc` |
| normative-tier-window-mm | `cpt-cf-bss-rating-design-tier-window-mm` |
| normative-unit-kinds-syn | `cpt-cf-bss-rating-design-unit-kinds-syn` |
| normative-usage-dedup-ing | `cpt-cf-bss-rating-design-usage-dedup-ing` |
| normative-usage-types-ing | `cpt-cf-bss-rating-design-usage-types-ing` |
| normative-waterfall-cmt | `cpt-cf-bss-rating-design-waterfall-cmt` |
