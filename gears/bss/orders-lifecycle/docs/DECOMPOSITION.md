# Decomposition: Orders Lifecycle

**Overall implementation status:**
- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-status-overall`

<!-- toc -->

- [1. Overview](#1-overview)
- [2. Entries](#2-entries)
  - [2.1 Order Transition Engine - HIGH](#21-order-transition-engine---high)
  - [2.2 Order and Line Capture - HIGH](#22-order-and-line-capture---high)
  - [2.3 Submit Gate and Price Pinning - HIGH](#23-submit-gate-and-price-pinning---high)
  - [2.4 Amendment and Version History - HIGH](#24-amendment-and-version-history---high)
  - [2.5 Acceptance and Payment Preconditions - HIGH](#25-acceptance-and-payment-preconditions---high)
  - [2.6 Workflow Integration - HIGH](#26-workflow-integration---high)
  - [2.7 Hold, Cancellation and Expiry - HIGH](#27-hold-cancellation-and-expiry---high)
  - [2.8 Reads and Authorization - HIGH](#28-reads-and-authorization---high)
- [3. Feature Dependencies](#3-feature-dependencies)
  - [3.1 Upstream and release prerequisites](#31-upstream-and-release-prerequisites)
  - [3.2 Selected reconciliation fixes and delivery sequence](#32-selected-reconciliation-fixes-and-delivery-sequence)
- [4. Contract address index](#4-contract-address-index)
  - [R04 implementation work (D-190; not yet delivered)](#r04-implementation-work-d-190-not-yet-delivered)
  - [R05 implementation work (D-191; not yet delivered)](#r05-implementation-work-d-191-not-yet-delivered)
  - [R06 implementation work (D-192; not yet delivered)](#r06-implementation-work-d-192-not-yet-delivered)
  - [R07 implementation dependency — billing-terms producer (D-193)](#r07-implementation-dependency--billing-terms-producer-d-193)
  - [R08 implementation dependency — service authentication and grants (D-194)](#r08-implementation-dependency--service-authentication-and-grants-d-194)
  - [R09 implementation dependency — diagnostic contract and mapping (D-195)](#r09-implementation-dependency--diagnostic-contract-and-mapping-d-195)
  - [R10 implementation dependency — revision reference lifetime (D-196)](#r10-implementation-dependency--revision-reference-lifetime-d-196)
  - [R11 implementation dependency — Rating exact-binding evaluation (D-197)](#r11-implementation-dependency--rating-exact-binding-evaluation-d-197)
  - [R13 implementation dependency — commercial authorities (D-199)](#r13-implementation-dependency--commercial-authorities-d-199)
  - [R14 implementation dependency — integrate the existing event platform (D-200)](#r14-implementation-dependency--integrate-the-existing-event-platform-d-200)

<!-- /toc -->

## 1. Overview

The design is decomposed into one shared Order Transition Engine and seven
capability features, following [ADR-0002](ADR/0002-cpt-cf-bss-orders-lifecycle-adr-slice-decomposition.md).
Each feature is an implementation and test boundary inside the same Gear. Features are
not independently deployable services.

[DESIGN.md](DESIGN.md) (`cpt-cf-bss-orders-lifecycle-design-orders-lifecycle`) retains architecture,
schemas, operation contracts and decision rationale. This document owns feature scope,
requirements allocation and build order. The linked `features/` documents describe flows,
processes, states, definitions of done and acceptance criteria. Existing design IDs remain
unchanged; feature IDs identify the implementation contracts derived from them.

All implementation checkboxes remain unchecked: authored specifications are not evidence
of running code. Open decisions in [DECISIONS.md](DECISIONS.md) and dependencies in
[UPSTREAM_REQS.md](UPSTREAM_REQS.md) remain open. This restructuring does not resolve them.

**Coverage policy.** Each of the PRD's 22 functional and seven non-functional requirements
has one primary feature owner below. Supporting features still inherit the engine's
invariants and participate in integration tests. Shared principles, entities and constraints
may appear in several entries; this means distinct feature-local obligations, not duplicate
ownership of an architectural definition. Foundation owns the shared persistence and public
contract infrastructure. Capability features own their contributions to those contracts.

**Authorization prerequisite.** The read feature's later delivery phase does not defer
write-path authorization. Foundation must integrate the authorization pre-guard and the
permission contract in [DESIGN.md](DESIGN.md#contract-08-4-3) before any mutating operation is exposed.
Read-and-authorization owns complete read surfaces and permission-matrix acceptance. Its
implementation dependency on earlier features is not a reverse dependency for that shared
security contract.

## 2. Entries

### 2.1 [Order Transition Engine](features/01-foundation.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-feature-foundation`

- **Purpose**: The four `p1` guarantees this gear is judged on — audit completeness, zero duplicate effects,
transition latency and recoverability — are properties of *how a state change commits*, not of
any single capability. Concentrating the commit in one component makes them assertable once and
unbreakable by a new slice.

- **Depends On**: None (feature-level). Platform prerequisites are listed in §3.

- **Scope**:
  - The order aggregate and its append-only version chain; the declarative state-machine table with its guards, terminal set, hold/resume mapping and expiry eligibility; guard evaluation and ordering; the idempotency registry and its non-success outcomes; the optimistic version check and its `version-conflict` refusal — the single registered name D-38 consolidated the `stale-version` variants into; the append-only transition audit; the typed event contract and the one-producer-message-per-event-declaring-transition rule; the registry of machine-readable business reasons; and the retention purge of the three bounded-retention stores (D-185).

- **Out of scope**:
  - It knows nothing commercial: not what a sellability predicate is, not what a price pin means, not whether an approval was warranted. It evaluates guards that slices declare, over document contributions that slices supply. It performs no money arithmetic, approval-policy evaluation, provisioning or commercial input resolution. Capability handlers own commercial input resolution; shared authorization and platform producer integration remain Foundation obligations, with external calls kept outside the transition transaction.

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-fr-order-state-machine`
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-fr-order-idempotency`
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-fr-order-events`
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-nfr-order-transition-latency`
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-nfr-order-audit-completeness`
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-nfr-order-idempotency`
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-nfr-order-recovery`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-principle-transition-through-engine`
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-principle-single-state-authority`
  - [ ] `p2` - `cpt-cf-bss-orders-lifecycle-principle-machine-readable-reasons`
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-principle-stored-idempotency`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-constraint-data-residency`
  - [ ] `p3` - `cpt-cf-bss-orders-lifecycle-constraint-categories-not-applicable`
  - [ ] `p3` - `cpt-cf-bss-orders-lifecycle-constraint-platform-baselines`

- **Domain Model Entities**:
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-entity-order-aggregate`
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-entity-order-version`
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-entity-transition`

- **Design Components**:

  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-component-transition-engine`
  - [ ] `p3` - `cpt-cf-bss-orders-lifecycle-tech-layering`
  - [ ] `p3` - `cpt-cf-bss-orders-lifecycle-topology-standard-bss-gear`
  - [ ] `p2` - `cpt-cf-bss-orders-lifecycle-interface-api-evolution`

- **API**:
  - Internal transition API, shared SDK and error/event contracts; no independently owned REST endpoint.
  - Shared contract: `cpt-cf-bss-orders-lifecycle-interface-order-operations`; PRD `cpt-cf-bss-orders-lifecycle-interface-order-ops`.

- **Sequences**:

  - None separately identified in DESIGN §3.6; feature flows cover this component.

- **Data**:
  - `cpt-cf-bss-orders-lifecycle-db-orders-store` — shared schema, transaction, migration and persistence infrastructure.
  - `orders_order`
  - `orders_order_version`
  - `orders_order_line_identity`
  - `orders_order_line`
  - `orders_draft_content`
  - `orders_order_admin`
  - `orders_order_line_admin`
  - `orders_resolved_total`
  - `orders_transition_audit`
  - `orders_audit_checkpoint`
  - `orders_audit_checkpoint_member`
  - `orders_idempotency`
  - `orders_line_fulfillment`
  - `orders_inflight_overlap_claim`
  - `orders_acceptance`

- **Detailed contract coverage**: [Architecture](DESIGN.md#contract-01-1-1). The following definitions belong to this feature in addition to the shared references above. Schemas, API definitions and architecture rationale remain normative in the linked design; flows and completion checks are in the feature specification.

  - **Source design**:
    - `cpt-cf-bss-orders-lifecycle-design-foundation`
  - **Technology**:
    - `cpt-cf-bss-orders-lifecycle-tech-foundation-stack`
  - **Principles**:
    - `cpt-cf-bss-orders-lifecycle-principle-atomic-transition-commit`
    - `cpt-cf-bss-orders-lifecycle-principle-guard-declared-not-embedded`
    - `cpt-cf-bss-orders-lifecycle-principle-outcome-store-idempotency`
    - `cpt-cf-bss-orders-lifecycle-principle-append-only-history`
    - `cpt-cf-bss-orders-lifecycle-principle-absence-is-refusal`
  - **Constraints**:
    - `cpt-cf-bss-orders-lifecycle-constraint-single-writer`
    - `cpt-cf-bss-orders-lifecycle-constraint-idempotency-window`
    - `cpt-cf-bss-orders-lifecycle-constraint-outbox-at-least-once`
    - `cpt-cf-bss-orders-lifecycle-constraint-event-consumer-contract`
    - `cpt-cf-bss-orders-lifecycle-constraint-guard-input-ports`
    - `cpt-cf-bss-orders-lifecycle-constraint-db-namespace`
  - **Entities**:
    - `cpt-cf-bss-orders-lifecycle-entity-order-root`
    - `cpt-cf-bss-orders-lifecycle-entity-order-version-chain`
    - `cpt-cf-bss-orders-lifecycle-entity-order-line-identity`
    - `cpt-cf-bss-orders-lifecycle-entity-order-line`
    - `cpt-cf-bss-orders-lifecycle-entity-resolved-total`
    - `cpt-cf-bss-orders-lifecycle-entity-administrative-content`
    - `cpt-cf-bss-orders-lifecycle-entity-transition-record`
    - `cpt-cf-bss-orders-lifecycle-entity-idempotency-record`
    - `cpt-cf-bss-orders-lifecycle-entity-outbox-entry`
  - **Components**:
    - `cpt-cf-bss-orders-lifecycle-component-transition-orchestrator`
    - `cpt-cf-bss-orders-lifecycle-component-guard-registry`
    - `cpt-cf-bss-orders-lifecycle-component-state-table`
    - `cpt-cf-bss-orders-lifecycle-component-idempotency-registry`
    - `cpt-cf-bss-orders-lifecycle-component-audit-store`
    - `cpt-cf-bss-orders-lifecycle-component-outbox-publisher`
    - `cpt-cf-bss-orders-lifecycle-component-reason-registry`
    - `cpt-cf-bss-orders-lifecycle-component-retention-purge`
  - **Interfaces**:
    - `cpt-cf-bss-orders-lifecycle-interface-transition-api`
    - `cpt-cf-bss-orders-lifecycle-interface-guard-registration`
    - `cpt-cf-bss-orders-lifecycle-interface-order-read-model`
  - **Sequences**:
    - `cpt-cf-bss-orders-lifecycle-seq-transition-commit`
    - `cpt-cf-bss-orders-lifecycle-seq-create-transition`
    - `cpt-cf-bss-orders-lifecycle-seq-idempotent-replay`
    - `cpt-cf-bss-orders-lifecycle-seq-outbox-drain`
  - **Database**:
    - `cpt-cf-bss-orders-lifecycle-db-foundation-schema`
  - **Tables**:
    - `cpt-cf-bss-orders-lifecycle-dbtable-event-outbox`
    - `cpt-cf-bss-orders-lifecycle-dbtable-order`
    - `cpt-cf-bss-orders-lifecycle-dbtable-order-version`
    - `cpt-cf-bss-orders-lifecycle-dbtable-order-line-identity`
    - `cpt-cf-bss-orders-lifecycle-dbtable-order-line`
    - `cpt-cf-bss-orders-lifecycle-dbtable-inflight-overlap-claim`
    - `cpt-cf-bss-orders-lifecycle-dbtable-draft-content`
    - `cpt-cf-bss-orders-lifecycle-dbtable-administrative-content`
    - `cpt-cf-bss-orders-lifecycle-dbtable-resolved-total`
    - `cpt-cf-bss-orders-lifecycle-dbtable-transition-audit`
    - `cpt-cf-bss-orders-lifecycle-dbtable-audit-checkpoint`
    - `cpt-cf-bss-orders-lifecycle-dbtable-audit-checkpoint-member`
    - `cpt-cf-bss-orders-lifecycle-dbtable-idempotency`
    - `cpt-cf-bss-orders-lifecycle-dbtable-line-fulfillment`
    - `cpt-cf-bss-orders-lifecycle-dbtable-acceptance`
  - **Deployment**:
    - `cpt-cf-bss-orders-lifecycle-topology-foundation-runtime`
  - **States**:
    - `cpt-cf-bss-orders-lifecycle-state-order-lifecycle`

- **Phase**: 0/1; [detailed design](DESIGN.md#contract-01-1-1). The `retention-purge` worker (`cpt-cf-bss-orders-lifecycle-component-retention-purge`) and the per-caller api-gateway limiter zone ship in this phase: refusal auditing is not enabled without the worker that bounds it (D-185).

- **Event contract**: `cpt-cf-bss-orders-lifecycle-contract-order-events`; eleven typed events and the existing event-less transition classes, with platform producer delivery acceptance; consumers meet the [event consumer contract](DESIGN.md#contract-01-event-consumer-contract) and its `orders-events` corpus gates their integration sign-off (D-186).

---

### 2.2 [Order and Line Capture](features/02-capture.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-feature-capture`

- **Purpose**: A basket must be assemblable without paying validation cost, and the line model must carry the
quoted commercial shape of the deal — term and cycle — or that shape is lost between order and
subscription.

- **Depends On**: `cpt-cf-bss-orders-lifecycle-feature-foundation`

- **Scope**:
  - Order and line authoring in `draft`; the line model including the mandatory contract-effective date, the optional service-activation and acceptance-due dates with their cascading defaults, term duration, billing cycle and external references; the single-currency basket rule; and the field-level classification of commercial versus administrative content.

- **Out of scope**:
  - It evaluates no sellability predicate, captures no pin, and computes no total — a `draft` is deliberately unvalidated. It does not decide whether a missing required date blocks submit; that guard belongs to the gate.

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-fr-order-create`
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-fr-order-line-dates`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-principle-content-immutability-split`

- **Design Constraints Covered**:

  - [ ] `p3` - `cpt-cf-bss-orders-lifecycle-constraint-platform-baselines`

- **Domain Model Entities**:
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-entity-order-line`
  - [ ] `p2` - `cpt-cf-bss-orders-lifecycle-entity-administrative-content-view`

- **Design Components**:

  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-component-capture`

- **API**:
  - `POST /bss-orders-lifecycle/v1/orders`
  - `PATCH /bss-orders-lifecycle/v1/orders/{orderId}`
  - `POST /bss-orders-lifecycle/v1/orders/{orderId}/lines`
  - `PATCH /bss-orders-lifecycle/v1/orders/{orderId}/lines/{lineId}`
  - `DELETE /bss-orders-lifecycle/v1/orders/{orderId}/lines/{lineId}`
  - Shared contract: `cpt-cf-bss-orders-lifecycle-interface-order-operations`; PRD `cpt-cf-bss-orders-lifecycle-interface-order-ops`.

- **Sequences**:

  - None separately identified in DESIGN §3.6; feature flows cover this component.

- **Data**:
  - `cpt-cf-bss-orders-lifecycle-db-orders-store` — feature-local content and constraints; engine write ownership is unchanged.
  - `orders_order_line_identity`
  - `orders_order_line`
  - `orders_draft_content`
  - `orders_order_admin`
  - `orders_order_line_admin`
  - `orders_date_policy`

- **Detailed contract coverage**: [Architecture](DESIGN.md#contract-02-1-1). The following definitions belong to this feature in addition to the shared references above. Schemas, API definitions and architecture rationale remain normative in the linked design; flows and completion checks are in the feature specification.

  - **Source design**:
    - `cpt-cf-bss-orders-lifecycle-design-capture`
  - **Principles**:
    - `cpt-cf-bss-orders-lifecycle-principle-draft-is-unvalidated`
    - `cpt-cf-bss-orders-lifecycle-principle-line-identity-stable`
    - `cpt-cf-bss-orders-lifecycle-principle-field-class-declared`
  - **Constraints**:
    - `cpt-cf-bss-orders-lifecycle-constraint-single-currency-basket`
    - `cpt-cf-bss-orders-lifecycle-constraint-change-category-refused`
    - `cpt-cf-bss-orders-lifecycle-constraint-single-payer`
    - `cpt-cf-bss-orders-lifecycle-constraint-no-addon-selection`
  - **Entities**:
    - `cpt-cf-bss-orders-lifecycle-entity-line-date-set`
  - **Components**:
    - `cpt-cf-bss-orders-lifecycle-component-capture-line-model`
    - `cpt-cf-bss-orders-lifecycle-component-capture-field-classifier`
  - **Interfaces**:
    - `cpt-cf-bss-orders-lifecycle-interface-capture-ops`
  - **Sequences**:
    - `cpt-cf-bss-orders-lifecycle-seq-create-draft`
    - `cpt-cf-bss-orders-lifecycle-seq-author-line`
    - `cpt-cf-bss-orders-lifecycle-seq-edit-order`
    - `cpt-cf-bss-orders-lifecycle-seq-edit-or-remove-line`
  - **Tables**:
    - `cpt-cf-bss-orders-lifecycle-dbtable-date-policy`

- **Phase**: 1; [detailed design](DESIGN.md#contract-02-1-1).

---

### 2.3 [Submit Gate and Price Pinning](features/03-gate-and-pin.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-feature-gate-and-pin`

- **Purpose**: Price integrity between capture and subscription activation is the revenue-integrity risk this
gear exists to close, and the gate is the only place it can be closed atomically with the state
change.

- **Depends On**: `cpt-cf-bss-orders-lifecycle-feature-foundation`, `cpt-cf-bss-orders-lifecycle-feature-capture`

- **Scope**:
  - The submit gate: the published pricing sellability predicates adopted by reference plus the Orders delta — tenant-axis validity, contract-active where referenced, purchase-quantity floor, order-market consistency against the payer's profile, reference resolution, single currency, overlap-rule uniqueness and the one-in-flight-order rule. Capture of the accepted order pin on every line and of the non-authoritative resolved total. The Preview operation, which creates no order or commercial artifact, persists bounded-retention gate outcomes and returns no approval verdict.

- **Out of scope**:
  - It does not author the adopted predicates and must never fork them. It computes no price — the total arrives from the price-evaluation contract and is stored as received. It returns no approval-requirement verdict, in Preview or at submit.

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-fr-order-submit`
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-fr-order-tenant-axes`
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-fr-orders-boundary-r4-no-price`
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-nfr-order-snapshot-integrity`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-principle-fail-closed`

- **Design Constraints Covered**:

  - [ ] `p2` - `cpt-cf-bss-orders-lifecycle-constraint-no-money-arithmetic`
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-constraint-unagreed-subscription-seams`

- **Domain Model Entities**:
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-entity-order-line`
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-entity-resolved-total-view`

- **Design Components**:

  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-component-gate-and-pin`

- **API**:
  - `POST /bss-orders-lifecycle/v1/orders/preview`
  - `POST /bss-orders-lifecycle/v1/orders/{orderId}/submit`
  - Shared contract: `cpt-cf-bss-orders-lifecycle-interface-order-operations`; PRD `cpt-cf-bss-orders-lifecycle-interface-order-ops`.

- **Sequences**:

  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-seq-submit-gate`

- **Data**:
  - `cpt-cf-bss-orders-lifecycle-db-orders-store` — feature-local content and constraints; engine write ownership is unchanged.
  - `orders_resolved_total`
  - `orders_inflight_overlap_claim`
  - `orders_gate_outcome`

- **Detailed contract coverage**: [Architecture](DESIGN.md#contract-03-1-1). The following definitions belong to this feature in addition to the shared references above. Schemas, API definitions and architecture rationale remain normative in the linked design; flows and completion checks are in the feature specification.

  - **Source design**:
    - `cpt-cf-bss-orders-lifecycle-design-gate-and-pin`
  - **Principles**:
    - `cpt-cf-bss-orders-lifecycle-principle-adopt-not-fork-gate`
    - `cpt-cf-bss-orders-lifecycle-principle-pin-is-the-commit`
    - `cpt-cf-bss-orders-lifecycle-principle-resolve-outside-decide-inside`
    - `cpt-cf-bss-orders-lifecycle-principle-preview-shares-implementation`
  - **Constraints**:
    - `cpt-cf-bss-orders-lifecycle-constraint-port-budgets`
    - `cpt-cf-bss-orders-lifecycle-constraint-partial-predicate-evaluability`
    - `cpt-cf-bss-orders-lifecycle-constraint-overlap-read-unagreed`
    - `cpt-cf-bss-orders-lifecycle-constraint-overlap-key-partner-collision`
    - `cpt-cf-bss-orders-lifecycle-constraint-total-excludes-subscription-overlays`
  - **Entities**:
    - `cpt-cf-bss-orders-lifecycle-entity-catalog-price-pin`
    - `cpt-cf-bss-orders-lifecycle-entity-order-market`
    - `cpt-cf-bss-orders-lifecycle-entity-gate-outcome`
  - **Components**:
    - `cpt-cf-bss-orders-lifecycle-component-gate-predicate-orchestrator`
    - `cpt-cf-bss-orders-lifecycle-component-gate-pin-capture`
    - `cpt-cf-bss-orders-lifecycle-component-gate-preview`
  - **Interfaces**:
    - `cpt-cf-bss-orders-lifecycle-interface-gate-ops`
    - `cpt-cf-bss-orders-lifecycle-interface-gate-ports`
  - **Sequences**:
    - `cpt-cf-bss-orders-lifecycle-seq-gate-submit`
    - `cpt-cf-bss-orders-lifecycle-seq-gate-preview`
    - `cpt-cf-bss-orders-lifecycle-seq-gate-fulfillment-recheck`
  - **Tables**:
    - `cpt-cf-bss-orders-lifecycle-dbtable-gate-outcome`

- **Phase**: 1; [detailed design](DESIGN.md#contract-03-1-1).

---

### 2.4 [Amendment and Version History](features/04-versioning.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-feature-versioning`

- **Purpose**: A commercial change before fulfillment must leave evidence of what changed, who changed it and
what it replaced, without mutating what a reviewer already saw.

- **Depends On**: `cpt-cf-bss-orders-lifecycle-feature-foundation`, `cpt-cf-bss-orders-lifecycle-feature-capture`, `cpt-cf-bss-orders-lifecycle-feature-gate-and-pin`

- **Scope**:
  - The amendment path from `submitted`, `pending_approval` and `approved`; the new-version append with its `supersedesVersion` reference; the gate re-run and re-pin trigger; historical version retrieval; the non-versioned audited administrative edit path; and the absence of any amendment row from `in_fulfillment` onward (engine `not-admissible`; [04 §2.2](DESIGN.md#contract-04-2-2) *Amendment stops at `in_fulfillment`*).

- **Out of scope**:
  - It does not decide whether the amended order needs re-approval — that verdict is external and arrives through the workflow seam. It does not delete or rewrite a prior version under any condition.

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-fr-order-amendment`
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-fr-order-history`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-principle-content-immutability-split`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-constraint-external-approval-policy`

- **Domain Model Entities**:
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-entity-order-version`
  - [ ] `p2` - `cpt-cf-bss-orders-lifecycle-entity-administrative-content-view`

- **Design Components**:

  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-component-versioning`

- **API**:
  - `POST /bss-orders-lifecycle/v1/orders/{orderId}/amendments`
  - Shared contract: `cpt-cf-bss-orders-lifecycle-interface-order-operations`; PRD `cpt-cf-bss-orders-lifecycle-interface-order-ops`.

- **Sequences**:

  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-seq-amendment-supersession`

- **Data**:
  - `cpt-cf-bss-orders-lifecycle-db-orders-store` — feature-local content and constraints; engine write ownership is unchanged.
  - `orders_order_version`

- **Detailed contract coverage**: [Architecture](DESIGN.md#contract-04-1-1). The following definitions belong to this feature in addition to the shared references above. Schemas, API definitions and architecture rationale remain normative in the linked design; flows and completion checks are in the feature specification.

  - **Source design**:
    - `cpt-cf-bss-orders-lifecycle-design-versioning`
  - **Principles**:
    - `cpt-cf-bss-orders-lifecycle-principle-version-is-concurrency`
    - `cpt-cf-bss-orders-lifecycle-principle-amend-by-append`
    - `cpt-cf-bss-orders-lifecycle-principle-carry-forward-reresolve`
    - `cpt-cf-bss-orders-lifecycle-principle-amendment-not-state-first`
  - **Constraints**:
    - `cpt-cf-bss-orders-lifecycle-constraint-no-amendment-in-fulfillment`
    - `cpt-cf-bss-orders-lifecycle-constraint-reapproval-target-external`
    - `cpt-cf-bss-orders-lifecycle-constraint-paired-payer-seller-rebinding`
  - **Entities**:
    - `cpt-cf-bss-orders-lifecycle-entity-amendment-request`
    - `cpt-cf-bss-orders-lifecycle-entity-administrative-edit`
  - **Components**:
    - `cpt-cf-bss-orders-lifecycle-component-versioning-appender`
    - `cpt-cf-bss-orders-lifecycle-component-versioning-reader`
  - **Interfaces**:
    - `cpt-cf-bss-orders-lifecycle-interface-versioning-ops`
  - **Sequences**:
    - `cpt-cf-bss-orders-lifecycle-seq-amend-order`
    - `cpt-cf-bss-orders-lifecycle-seq-administrative-edit`

- **Phase**: 2; [detailed design](DESIGN.md#contract-04-1-1).

---

### 2.5 [Acceptance and Payment Preconditions](features/05-preconditions.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-feature-preconditions`

- **Purpose**: On a partner-placed order the document evidences delegation but not agreement, and without a
money gate before provisioning a non-paying tenant receives resources.

- **Depends On**: `cpt-cf-bss-orders-lifecycle-feature-foundation`, `cpt-cf-bss-orders-lifecycle-feature-capture`, `cpt-cf-bss-orders-lifecycle-feature-gate-and-pin`

- **Scope**:
  - The customer-acceptance instant as a recorded fact with its own transition and event; the source of the acceptance-required election; and the begin-fulfillment guard inputs — recorded acceptance where required, and the payment-authorization outcome with the seller tolerate-failure election and its risk flag.

- **Out of scope**:
  - It owns no payment mechanism and holds no instrument data. It never defaults the acceptance instant, under any policy, including the cascade that fills the line-level acceptance-due date.

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-fr-order-acceptance`
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-fr-order-payment-auth`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-principle-fail-closed`

- **Design Constraints Covered**:

  - [ ] `p2` - `cpt-cf-bss-orders-lifecycle-constraint-payment-ordering`

- **Domain Model Entities**:
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-entity-acceptance`

- **Design Components**:

  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-component-preconditions`

- **API**:
  - `POST /bss-orders-lifecycle/v1/orders/{orderId}/acceptance`
  - `GET /bss-orders-lifecycle/v1/orders/{orderId}/acceptance`
  - Shared contract: `cpt-cf-bss-orders-lifecycle-interface-order-operations`; PRD `cpt-cf-bss-orders-lifecycle-interface-order-ops`.

- **Sequences**:

  - None separately identified in DESIGN §3.6; feature flows cover this component.

- **Data**:
  - `cpt-cf-bss-orders-lifecycle-db-orders-store` — feature-local content and constraints; engine write ownership is unchanged.
  - `orders_acceptance`
  - `orders_policy_election`

- **Detailed contract coverage**: [Architecture](DESIGN.md#contract-05-1-1). The following definitions belong to this feature in addition to the shared references above. Schemas, API definitions and architecture rationale remain normative in the linked design; flows and completion checks are in the feature specification.

  - **Source design**:
    - `cpt-cf-bss-orders-lifecycle-design-preconditions`
  - **Principles**:
    - `cpt-cf-bss-orders-lifecycle-principle-acceptance-never-defaulted`
    - `cpt-cf-bss-orders-lifecycle-principle-agreement-not-delegation`
    - `cpt-cf-bss-orders-lifecycle-principle-authorization-read-not-owned`
  - **Constraints**:
    - `cpt-cf-bss-orders-lifecycle-constraint-no-payment-pending-state`
    - `cpt-cf-bss-orders-lifecycle-constraint-declined-instrument-exit`
    - `cpt-cf-bss-orders-lifecycle-constraint-no-payment-collection`
  - **Entities**:
    - `cpt-cf-bss-orders-lifecycle-entity-acceptance-record`
    - `cpt-cf-bss-orders-lifecycle-entity-authorization-outcome`
  - **Components**:
    - `cpt-cf-bss-orders-lifecycle-component-preconditions-acceptance`
    - `cpt-cf-bss-orders-lifecycle-component-preconditions-money-gate`
  - **Interfaces**:
    - `cpt-cf-bss-orders-lifecycle-interface-preconditions-ops`
  - **Sequences**:
    - `cpt-cf-bss-orders-lifecycle-seq-record-acceptance`
    - `cpt-cf-bss-orders-lifecycle-seq-self-service-acceptance`
    - `cpt-cf-bss-orders-lifecycle-seq-begin-fulfillment-guards`
  - **Tables**:
    - `cpt-cf-bss-orders-lifecycle-dbtable-policy-election`

- **Phase**: 2; [detailed design](DESIGN.md#contract-05-1-1).

---

### 2.6 [Workflow Integration](features/06-workflow-seam.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-feature-workflow-seam`

- **Purpose**: R1 through R5 are only real if the operations the sibling gear calls are ordinary guarded
transitions rather than privileged state assertions.

- **Depends On**: `cpt-cf-bss-orders-lifecycle-feature-foundation`, `cpt-cf-bss-orders-lifecycle-feature-gate-and-pin`, `cpt-cf-bss-orders-lifecycle-feature-preconditions`

- **Scope**:
  - The five workflow-only operations — approval reflection, begin fulfillment, spawn-signal report, fulfillment acknowledgement, and workflow-mediated cancel with attached compensation evidence; the recorded spawn signal that anchors the cancel guard; the persisted per-line line-to-subscription linkage; the read-only per-line fulfillment projection; and the recorded deciding authority on every stored verdict.

- **Out of scope**:
  - It implements no approval logic, no retry, no compensation and no provisioning. It never mirrors the downstream `TransitionRequest` status, and it never derives order state from a stored transition-request identifier.

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-fr-order-atomic-fulfillment`
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-fr-order-subscription-linkage`
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-fr-orders-boundary-r1-state-sor`
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-fr-orders-boundary-r2-approval`
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-fr-orders-boundary-r3-provisioning`
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-fr-orders-boundary-r5-no-mirroring`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-principle-single-state-authority`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-constraint-external-approval-policy`
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-constraint-unagreed-subscription-seams`

- **Domain Model Entities**:
  - [ ] `p2` - `cpt-cf-bss-orders-lifecycle-entity-line-fulfillment`

- **Design Components**:

  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-component-workflow-seam`

- **API**:
  - `POST /bss-orders-lifecycle/v1/orders/{orderId}/approval-reflection`
  - `POST /bss-orders-lifecycle/v1/orders/{orderId}/begin-fulfillment`
  - `POST /bss-orders-lifecycle/v1/orders/{orderId}/spawn-signal`
  - `POST /bss-orders-lifecycle/v1/orders/{orderId}/fulfillment-acknowledgement`
  - `POST /bss-orders-lifecycle/v1/orders/{orderId}/workflow-cancel`
  - Shared contract: `cpt-cf-bss-orders-lifecycle-interface-order-operations`; PRD `cpt-cf-bss-orders-lifecycle-interface-order-ops`.

- **Sequences**:

  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-seq-fulfillment-acknowledgement`
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-seq-cancel-guard`

- **Data**:
  - `cpt-cf-bss-orders-lifecycle-db-orders-store` — feature-local content and constraints; engine write ownership is unchanged.
  - `orders_line_fulfillment`
  - `orders_approval_reflection`

- **Detailed contract coverage**: [Architecture](DESIGN.md#contract-06-1-1). The following definitions belong to this feature in addition to the shared references above. Schemas, API definitions and architecture rationale remain normative in the linked design; flows and completion checks are in the feature specification.

  - **Source design**:
    - `cpt-cf-bss-orders-lifecycle-design-workflow-seam`
  - **Principles**:
    - `cpt-cf-bss-orders-lifecycle-principle-workflow-is-ordinary-caller`
    - `cpt-cf-bss-orders-lifecycle-principle-store-verdict-not-reasoning`
    - `cpt-cf-bss-orders-lifecycle-principle-commit-anchor-before-risk`
    - `cpt-cf-bss-orders-lifecycle-principle-outcome-not-mirror`
  - **Constraints**:
    - `cpt-cf-bss-orders-lifecycle-constraint-approval-owner-absent`
    - `cpt-cf-bss-orders-lifecycle-constraint-compensation-reason-unagreed`
    - `cpt-cf-bss-orders-lifecycle-constraint-provenance-one-directional`
    - `cpt-cf-bss-orders-lifecycle-constraint-correlation-propagation-unagreed`
  - **Entities**:
    - `cpt-cf-bss-orders-lifecycle-entity-approval-reflection`
    - `cpt-cf-bss-orders-lifecycle-entity-spawn-signal`
    - `cpt-cf-bss-orders-lifecycle-entity-fulfillment-acknowledgement`
    - `cpt-cf-bss-orders-lifecycle-entity-line-fulfillment-projection`
  - **Components**:
    - `cpt-cf-bss-orders-lifecycle-component-seam-verdict-reflector`
    - `cpt-cf-bss-orders-lifecycle-component-seam-fulfillment-coordinator`
    - `cpt-cf-bss-orders-lifecycle-component-seam-line-projection`
  - **Interfaces**:
    - `cpt-cf-bss-orders-lifecycle-interface-seam-ops`
  - **Sequences**:
    - `cpt-cf-bss-orders-lifecycle-seq-reflect-verdict`
    - `cpt-cf-bss-orders-lifecycle-seq-begin-and-spawn`
    - `cpt-cf-bss-orders-lifecycle-seq-acknowledge-fulfillment`
    - `cpt-cf-bss-orders-lifecycle-seq-seam-cancel-guard`
    - `cpt-cf-bss-orders-lifecycle-seq-workflow-cancel`
  - **Tables**:
    - `cpt-cf-bss-orders-lifecycle-dbtable-approval-reflection`

- **Phase**: 2; [detailed design](DESIGN.md#contract-06-1-1).

---

### 2.7 [Hold, Cancellation and Expiry](features/07-hold-and-expiry.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-feature-hold-and-expiry`

- **Purpose**: An unbounded in-flight commercial state pins a price, holds an open promise to a customer and
accumulates operational debt — while an order whose subscriptions may already be provisioning
cannot be closed automatically.

- **Depends On**: `cpt-cf-bss-orders-lifecycle-feature-foundation`, `cpt-cf-bss-orders-lifecycle-feature-capture`, `cpt-cf-bss-orders-lifecycle-feature-workflow-seam`

- **Scope**:
  - Caller-initiated cancellation with the recorded spawn-signal guard, and abandoned-draft auto-void to `expired` without deleting commercial evidence.
  - Hold and resume with the stored pre-hold state; the per-state TTL policy for `submitted`, `pending_approval`, `approved` and `on_hold`; the **resume cap** that stops a hold/resume cycle restarting the dwell without limit — its sibling, the amendment cap, is owned in [04 §4.1](features/04-versioning.md#contract-04-4-1) because its value is a commercial judgment; the coordinated expiry scheduler; and the transition-table exclusion of `in_fulfillment` and of holds taken from it, together with the handoff of those cases to the operational escalation owned by the sibling gear, the Orders-side overdue-fulfillment gauge and alert, and the two-person operator-forced `fulfillment_failed` that is the bounded end of that escalation (D-182). It supplies **no code-constant fallback** duration: every expirable state ships a provisional platform TTL as a migration-seeded, revisioned policy row, and the policy channel refuses an unset duration in production ([07 §4.2](features/07-hold-and-expiry.md#contract-07-4-2), [`DECISIONS.md`](./DECISIONS.md) D-181, closing Q-27).

- **Out of scope**:
  - It does not pause entitlement or billing on already-activated subscriptions, does not extend a term, and does not void wave-1 subscription drafts. It never auto-terminals an order whose fulfillment may be in flight; the forced exit is taken only by two fulfillment operators after the overdue window, never by a sweep.

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-fr-order-cancel`
  - [ ] `p2` - `cpt-cf-bss-orders-lifecycle-fr-order-hold`
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-fr-order-expiry`
  - [ ] `p2` - `cpt-cf-bss-orders-lifecycle-nfr-order-retention`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-principle-transition-through-engine`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-constraint-data-residency`

- **Domain Model Entities**:
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-entity-order-aggregate`

- **Design Components**:

  - [ ] `p2` - `cpt-cf-bss-orders-lifecycle-component-hold-and-expiry`

- **API**:
  - `POST /bss-orders-lifecycle/v1/orders/{orderId}/cancel`
  - `POST /bss-orders-lifecycle/v1/orders/{orderId}/hold`
  - `POST /bss-orders-lifecycle/v1/orders/{orderId}/resume`
  - `POST /bss-orders-lifecycle/v1/orders/{orderId}/forced-failure`
  - Shared contract: `cpt-cf-bss-orders-lifecycle-interface-order-operations`; PRD `cpt-cf-bss-orders-lifecycle-interface-order-ops`.

- **Sequences**:

  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-seq-cancel-guard`
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-seq-state-expiry`

- **Data**:
  - `cpt-cf-bss-orders-lifecycle-db-orders-store` — feature-local content and constraints; engine write ownership is unchanged.
  - `orders_state_ttl_policy`

- **Detailed contract coverage**: [Architecture](DESIGN.md#contract-07-1-1). The following definitions belong to this feature in addition to the shared references above. Schemas, API definitions and architecture rationale remain normative in the linked design; flows and completion checks are in the feature specification.

  - **Source design**:
    - `cpt-cf-bss-orders-lifecycle-design-hold-and-expiry`
  - **Principles**:
    - `cpt-cf-bss-orders-lifecycle-principle-hold-changes-only-order`
    - `cpt-cf-bss-orders-lifecycle-principle-prehold-stored`
    - `cpt-cf-bss-orders-lifecycle-principle-exemptions-in-table`
    - `cpt-cf-bss-orders-lifecycle-principle-resume-is-capped`
    - `cpt-cf-bss-orders-lifecycle-principle-park-does-not-stop-clock`
  - **Constraints**:
    - `cpt-cf-bss-orders-lifecycle-constraint-in-fulfillment-not-expirable`
    - `cpt-cf-bss-orders-lifecycle-constraint-hold-does-not-pause-draft-ttl`
    - `cpt-cf-bss-orders-lifecycle-constraint-ttl-values-unchosen`
  - **Entities**:
    - `cpt-cf-bss-orders-lifecycle-entity-hold-record`
    - `cpt-cf-bss-orders-lifecycle-entity-state-ttl-policy`
    - `cpt-cf-bss-orders-lifecycle-entity-resume-cap`
  - **Components**:
    - `cpt-cf-bss-orders-lifecycle-component-hold-handler`
    - `cpt-cf-bss-orders-lifecycle-component-expiry-scheduler`
    - `cpt-cf-bss-orders-lifecycle-component-draft-sweep`
  - **Interfaces**:
    - `cpt-cf-bss-orders-lifecycle-interface-hold-ops`
  - **Sequences**:
    - `cpt-cf-bss-orders-lifecycle-seq-hold-resume`
    - `cpt-cf-bss-orders-lifecycle-seq-expiry-sweep`
    - `cpt-cf-bss-orders-lifecycle-seq-overdue-handoff`
    - `cpt-cf-bss-orders-lifecycle-seq-forced-unreconciled-failure`
  - **Tables**:
    - `cpt-cf-bss-orders-lifecycle-dbtable-state-ttl-policy`

- **Phase**: 2/3; [detailed design](DESIGN.md#contract-07-1-1).

---

### 2.8 [Reads and Authorization](features/08-read-and-authz.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-feature-read-and-authz`

- **Purpose**: Order consoles and downstream systems query state frequently against a strict read budget, and
cross-tenant leakage in a multi-tenant BSS gear is a critical confidentiality failure.

- **Depends On**: `cpt-cf-bss-orders-lifecycle-feature-foundation`, `cpt-cf-bss-orders-lifecycle-feature-capture`, `cpt-cf-bss-orders-lifecycle-feature-gate-and-pin`, `cpt-cf-bss-orders-lifecycle-feature-versioning`, `cpt-cf-bss-orders-lifecycle-feature-workflow-seam`

- **Scope**:
  - The current-version read projection and the paginated tenancy-scoped list with its state, date and contract filters; historical version reads; the exposure of expected fulfillment time and per-line deferral where the barrier deferred a line; audit-trail retrieval; and the per-actor permission set including the cross-tenant delegation-proof requirement.

- **Out of scope**:
  - It does not mutate order state and registers no transition; required read-access evidence is appended to `orders_read_access_log`. It never serves an order outside the caller's tenancy scope, and it never exposes internal diagnostics through a read surface.

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-fr-order-authorization`
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-nfr-order-read-latency`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-principle-fail-closed`

- **Design Constraints Covered**:

  - [ ] `p3` - `cpt-cf-bss-orders-lifecycle-constraint-platform-baselines`
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-constraint-data-residency`

- **Domain Model Entities**:
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-entity-order-aggregate`
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-entity-order-version`
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-entity-order-line`
  - [ ] `p1` - `cpt-cf-bss-orders-lifecycle-entity-resolved-total-view`
  - [ ] `p2` - `cpt-cf-bss-orders-lifecycle-entity-line-fulfillment`
  - [ ] `p2` - `cpt-cf-bss-orders-lifecycle-entity-administrative-content-view`

- **Design Components**:

  - [ ] `p2` - `cpt-cf-bss-orders-lifecycle-component-read-and-authz`

- **API**:
  - `GET /bss-orders-lifecycle/v1/orders`
  - `GET /bss-orders-lifecycle/v1/orders/{orderId}`
  - `GET /bss-orders-lifecycle/v1/orders/{orderId}/versions`
  - `GET /bss-orders-lifecycle/v1/orders/{orderId}/versions/{version}`
  - `GET /bss-orders-lifecycle/v1/orders/{orderId}/lines`
  - `GET /bss-orders-lifecycle/v1/orders/{orderId}/audit`
  - Shared contract: `cpt-cf-bss-orders-lifecycle-interface-order-operations`; PRD `cpt-cf-bss-orders-lifecycle-interface-order-ops`.

- **Sequences**:

  - None separately identified in DESIGN §3.6; feature flows cover this component.

- **Data**:
  - `cpt-cf-bss-orders-lifecycle-db-orders-store` — feature-local content and constraints; engine write ownership is unchanged.
  - `orders_read_access_log`

- **Detailed contract coverage**: [Architecture](DESIGN.md#contract-08-1-1). The following definitions belong to this feature in addition to the shared references above. Schemas, API definitions and architecture rationale remain normative in the linked design; flows and completion checks are in the feature specification.

  - **Source design**:
    - `cpt-cf-bss-orders-lifecycle-design-read-and-authz`
  - **Principles**:
    - `cpt-cf-bss-orders-lifecycle-principle-read-row-not-chain`
    - `cpt-cf-bss-orders-lifecycle-principle-scope-by-relationship`
    - `cpt-cf-bss-orders-lifecycle-principle-no-internal-exposure`
    - `cpt-cf-bss-orders-lifecycle-principle-one-permission-model`
  - **Constraints**:
    - `cpt-cf-bss-orders-lifecycle-constraint-read-fails-closed`
    - `cpt-cf-bss-orders-lifecycle-constraint-delegation-proof-required`
    - `cpt-cf-bss-orders-lifecycle-constraint-bounded-page-size`
  - **Entities**:
    - `cpt-cf-bss-orders-lifecycle-entity-order-read-view`
    - `cpt-cf-bss-orders-lifecycle-entity-permission-declaration`
  - **Components**:
    - `cpt-cf-bss-orders-lifecycle-component-read-projection`
    - `cpt-cf-bss-orders-lifecycle-component-authz-declaration`
  - **Interfaces**:
    - `cpt-cf-bss-orders-lifecycle-interface-read-ops`
  - **Sequences**:
    - `cpt-cf-bss-orders-lifecycle-seq-scoped-read`
    - `cpt-cf-bss-orders-lifecycle-seq-list-orders`
    - `cpt-cf-bss-orders-lifecycle-seq-audit-read`
  - **Tables**:
    - `cpt-cf-bss-orders-lifecycle-dbtable-read-access-log`

- **Phase**: 2/3; [detailed design](DESIGN.md#contract-08-1-1).

## 3. Feature Dependencies

The table is the build-order authority and preserves the accepted dependency edges. Numeric prefixes reflect implementation order,
not PRD section numbering. All features are HIGH priority because each carries at least
one `p1` obligation; delivery phases express sequencing rather than weakening priority.

| Feature | PRD area | Phase | Depends on |
|---------|----------|-------|------------|
| [01 foundation](features/01-foundation.md) | 6.1 state machine/idempotency; 6.5; 7.1 | 0/1 | Platform prerequisites below |
| [02 capture](features/02-capture.md) | 6.1 create and line dates | 1 | 01 |
| [03 gate-and-pin](features/03-gate-and-pin.md) | 6.1 submit; 9.1 Preview | 1 | 01, 02 |
| [04 versioning](features/04-versioning.md) | 6.2 | 2 | 01, 02, 03 |
| [05 preconditions](features/05-preconditions.md) | 6.1 acceptance and payment authorization | 2 | 01, 02, 03 |
| [06 workflow-seam](features/06-workflow-seam.md) | 6.1 fulfillment/linkage; 6.4 | 2 | 01, 03, 05 |
| [07 hold-and-expiry](features/07-hold-and-expiry.md) | 6.3 | 2/3 | 01, 02, 06 |
| [08 read-and-authz](features/08-read-and-authz.md) | 6.6; 9.1 reads | 2/3 | 01, 02, 03, 04, 06 |

**Dependency rationale and parallel work:**

- Foundation provides the atomic transition contract, persistence, authorization pre-guard,
  idempotency and event publication infrastructure used by every capability.
- Capture supplies the draft and line model consumed by gate-and-pin.
- Versioning and preconditions can proceed in parallel after foundation, capture and gate-and-pin.
  Versioning reuses gate resolution and pinning; preconditions contributes acceptance to submit.
- Workflow-seam consumes the gate's pinned snapshot and market binding and the preconditions
  guard inputs before beginning fulfillment.
- Hold-and-expiry consumes capture's draft content for auto-void and Workflow's stored spawn
  signal for cancellation guards.
- Read-and-authz consumes historical versions from versioning and the fulfillment projection
  from workflow-seam, as well as capture and pinned gate data. It can proceed alongside
  hold-and-expiry once its listed dependencies are ready.
- Shared authorization contracts are consumed by foundation at phase 0/1; public writes cannot
  ship with authorization postponed to phase 2/3. This is a contract prerequisite, not a cyclic
  dependency on the later read feature implementation.

### 3.1 Upstream and release prerequisites

**Phase 1's successful submit path has open upstream prerequisites.** The gate fails closed
on an unevaluable input ([`ADR/0003`](ADR/0003-cpt-cf-bss-orders-lifecycle-adr-fail-closed-gate.md)).
The PriceBook target is ADR-0008 / D-150–D-168. Pricing's resolve, pinned-price and plan reads
and the typed `PricingReadV1` provider exist on reviewed main (D-187). Orders still needs its
own SDK adapter and [required evidence conformance](DESIGN.md#contract-03-pricing-read-mapping),
authenticated seller-scoped Orders service provisioning and explicit least-privilege grants
(the D-194 call matrix), and the proposed nonbinding Pricing assessment with shared admission rules
(D-189); from Products, a SKU read grant for that subject. D-191 assigns the issued deadline to
Pricing receipts; authorized seller-policy discovery/resolution and deadline conformance remain
missing implementation prerequisites. No Orders-local formula may override D-190 price protection. Rating has adopted PriceBook but its pre-purchase/TCV
adapter is pending. Subscriptions needs the D-190 committed-receipt/hold/fresh-eligibility handoff,
D-192 frozen first-period mapping implementation, D-193 pre-subscription billing-terms resolver, the SUB-G1 key answer and the SUB-O5 count amendment (D-163),
`order_compensation` (SUB-O1), the order reference (SUB-O2), the start instant (SUB-O10) and atomic
activation (`…-upreq-overlap-activation-atomicity`). That last ask is a **release gate** (D-180):
until Subscriptions agrees and delivers it, subscription-side cardinality is advisory at order time
and the submit/activation path is not production-ready. SKU protection is inherited from the revision's references (D-164); D-196 assigns future release coordination to Pricing, with durable admission closure, drain and fenced Orders/Subscriptions usage proof. Current revision release remains deferred; a count query alone does not authorize release. D-194
service authorization is a separate prerequisite. Workflow needs complete topology and
the approval/payment owner contracts. The reciprocal amendments specify the missing shapes.

`activation_deadline` copies the selected receipt’s finite `hold_until` (D-191); no Orders interval
or state TTL replaces it. Pricing seller-policy resolution/discovery remains missing, and a
deadline pass is only an early check, not admission authority. Optional
totals and renewal-as-hold fallbacks are forbidden. Account Management payer-profile, Contracts and
platform event/authorization prerequisites remain open. Capture/engine development against doubles
may proceed, but successful production submit/activation requires producer implementation,
deployed authorization and end-to-end evidence separately.

The counterpart asks are registered one by one in [`UPSTREAM_REQS.md`](UPSTREAM_REQS.md); an
upstream ask is never equated with an implemented seam.

Phase 0/1 is the correctness core and is a prerequisite for everything else. Its event-producing
runtime uses the existing Event Broker implementation and managed producer (D-200); the
SDK-only platform inventory statement is stale. Production readiness still requires real provider
integration, all event registrations, root identity/grants, partition validation and proven recovery.
Startup must prepare all event types, resolve the managed chained producer and start toolkit outbox
workers before readiness. Capture and local
transition tests can proceed against an `EventBrokerApi` double; production event traffic cannot.
The four `p1` non-functional guarantees — audit completeness, zero duplicate effects, transition
latency and recoverability — are properties of the Engine, so no slice can be accepted before it
exists.
Phase 1 delivers the capture-to-`submitted` path, which is the smallest set that produces a
pinned, audited order. Phase 2 completes the commercial lifecycle and the sibling-gear seam.


Additional upstream contracts, unresolved product decisions and acceptance evidence remain
tracked in [UPSTREAM_REQS.md](UPSTREAM_REQS.md), [DECISIONS.md](DECISIONS.md), and each
feature’s canonical contract references. A double permits local development; it does not satisfy
a production dependency. No feature may mark itself implemented merely because its document exists.

### 3.2 Selected reconciliation fixes and delivery sequence

Agent-ready breakdown: implementation handoff, review and dispositions, and source coverage inventory. The six detailed plans contain 70 work packages with owner dependencies and completion evidence; stage numbers do not delay early upstream provider work.

On 2026-10-05 the user selected A for R01–R10 and authorized the best recommendation
for all remaining findings. R11–R14 also select A. These are **selected design contracts**,
not completed implementation checkboxes or evidence of another team's deployment/signoff.
The finding IDs below refer to the Pricing/Products reconciliation, not the older
hyphenated review-wave IDs in DECISIONS.

| Finding / decision | Selected fix | Delivery owner / remaining work |
|---|---|---|
| R01 / D-187 | Adapt the existing typed Pricing reads | Orders: SDK adapter, selection/evidence mapping and real-provider tests |
| R02 / D-188 | Reserve commercial versions before remote acceptance | Orders: durable attempts, immutable retries, receipt recovery and fenced final commit |
| R03 / D-189 | Nonbinding assessment for Preview | Pricing: shared evaluator/read API; Orders: Preview adapter without acceptance side effects |
| R04 / D-190 | Honor accepted prices through receipt/hold handoff | Subscriptions + Workflow: committed receipt, hold, fresh eligibility and receiver admission |
| R05 / D-191 | Pricing owns the commercial deadline | Pricing: seller-policy discovery/resolution; Orders: exact receipt deadline projection |
| R06 / D-192 | Freeze the complete accepted commercial inputs | Orders + Subscriptions + Rating: lossless schema-2 codec and downstream mapping |
| R07 / D-193 | Subscriptions resolves billing terms before purchase | Subscriptions: authorized resolver, policy provenance and exact period mapping |
| R08 / D-194 | Use authenticated service principals and explicit grants | Platform + owners: identities, seller-scoped permissions and denial tests |
| R09 / D-195 | Version diagnostics and verify complete coverage | Pricing + Orders: producer profile, mapping registry and honest partial-failure diagnostics |
| R10 / D-196 | Pricing coordinates safe reference release | Pricing + Orders + Subscriptions: admission closure, drain, fenced usage proof; retain references until delivered |
| R11 / D-197 | Rating evaluates exact bindings and all money | Rating: purchase evaluation, labelled horizons/TCV and supported-profile conformance |
| R12 / D-198 | Reserve receiver capacity and fence activation attempts | Subscriptions + Workflow + Orders: pending/active slots, barriers, cancellation and uncertain-outcome reconciliation |
| R13 / D-199 | Keep commercial facts with their owning providers | Account Management + Contracts + Payments + Workflow: typed facts, permissions and scope-specific readiness |
| R14 / D-200 | Integrate the existing broker and transactional outbox | Orders + Platform: schema registration, atomic enqueue, readiness and delivery/recovery evidence |

**Implementation batches.** These refine the existing feature phases, not replace them.
Each batch may contain multiple reviewable PRs; owner-provider work can proceed in parallel.

1. **Contracts and fixtures.** Fix SDK/profile versions, schemas, canonical digests, supported
   term/model scope, authority and error mappings for D-189/D-191/D-193/D-195/D-197/D-199.
   Record actual provider/grant readiness in UPSTREAM_REQS. Create shared receipt, terms,
   total and activation fixtures. Exit: executable contract fixtures and explicit owner
   dependencies; a double is labelled as such.
2. **Orders foundation and capture.** Implement scoped persistence, authorization, transition
   engine, audit/idempotency and draft capture. Include D-188 attempt/version storage and
   D-198 operational barrier storage in the migration plan. Integrate D-200 using the
   existing producer/outbox in the same transaction. Exit: refusal/rollback, retry,
   concurrent execution and lost-ack tests; no externally exposed write bypasses authorization.
3. **Provider delivery and gate adapters.** Build the missing Pricing assessment/policy,
   Subscriptions terms and Rating evaluation providers; wire D-187/D-194 adapters and
   D-195 diagnostics. Implement Preview first against these nonbinding contracts, then
   submit using D-188/D-192. Exit: complete diagnostics, no Preview acceptance writes,
   exact evaluated-versus-accepted input equality and recovery after partial multi-line
   acceptance. Production successful submit requires its real providers and grants.
4. **Versioning and commercial preconditions.** Implement amendment, current-version
   customer consent, approval and payment outcomes against D-199 owners. Exit: old
   receipts/verdicts/consent cannot authorize a replacement version; pending payment
   survives restart and outages never become tolerated business failure.
5. **Fulfillment and lifecycle controls.** Deliver the D-190/D-192 handoff with D-198
   receiver slots/fences before enabling activation. Implement Workflow compensation,
   post-spawn hold/cancel barriers, expiry and existing recovery workers together.
   Exit: concurrent admissions respect capacity, revoked attempts cannot newly commit
   activation, and ambiguous provisioning retains its capacity and recovery obligation.
   Exercise the explicitly documented force-fail-with-unknown-compensation exception.
6. **Read surfaces and release evidence.** Finish authorized reads/history and exclusions;
   run the full provider-backed lifecycle corpus, isolation tests, failure injection,
   delivery recovery and latency checks. D-196 cleanup may remain disabled with references
   retained; enabling release requires its separate closure/drain/zero-holder conformance.
   No feature is marked delivered solely from local doubles or specification completion.

Start runtime implementation with batch 1's contract fixtures and batch 2's foundation;
assign the missing owner providers in parallel. The production release gates in §3.1 and
UPSTREAM_REQS remain binding throughout.


## 4. Contract address index

Stable contract namespaces `01`–`08` retain the source section addresses used in decision
propagation and algorithm citations. The table resolves each address to its current canonical
section. A bare section reference inside a detailed contract uses that contract’s namespace;
explicit cross-contract citations name their namespace. These addresses are not file paths.

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
| 01 §3.6 | [Interactions and Sequences](features/01-foundation.md#contract-01-3-6) |
| 01 §3.7 | [Database Schemas and Tables](DESIGN.md#contract-01-3-7) |
| 01 §3.8 | [Deployment Topology](DESIGN.md#contract-01-3-8) |
| 01 §4.1 | [The Transition Contract (normative)](DESIGN.md#contract-01-4-1) |
| 01 §4.2 | [Idempotency Semantics (normative)](features/01-foundation.md#contract-01-4-2) |
| 01 §4.3 | [The State Machine (normative)](features/01-foundation.md#contract-01-4-3) |
| 01 §4.4 | [Events, Audit and the Outbox (normative)](DESIGN.md#contract-01-4-4) |
| 01 §4.5 | [What this slice deliberately does not own](features/01-foundation.md#contract-01-4-5) |
| 01 §4.6 | [Extension points and stability (normative)](DESIGN.md#contract-01-4-6) |
| 01 §4.7 | [GTS types for the cross-gear contract surface (normative)](DESIGN.md#contract-01-4-7) |
| 01 §4.8 | [What is deliberately not GTS](DESIGN.md#contract-01-4-8) |
| 01 §4.9 | [What this section changed, and why it is recorded](DESIGN.md#contract-01-4-9) |
| 01 §5 | [Traceability](features/01-foundation.md#contract-01-5) |
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
| 02 §3.6 | [Interactions and Sequences](features/02-capture.md#contract-02-3-6) |
| 02 §3.7 | [Database Schemas and Tables](DESIGN.md#contract-02-3-7) |
| 02 §3.8 | [Deployment Topology](DESIGN.md#contract-02-3-8) |
| 02 §4.1 | [What a draft may and may not hold (normative)](features/02-capture.md#contract-02-4-1) |
| 02 §4.2 | [The date cascade (normative)](features/02-capture.md#contract-02-4-2) |
| 02 §4.3 | [Field classification (normative)](DESIGN.md#contract-02-4-3) |
| 02 §4.4 | [Line shapes that are one line (normative)](DESIGN.md#contract-02-4-4) |
| 02 §4.5 | [Draft abandonment](features/02-capture.md#contract-02-4-5) |
| 02 §5 | [Traceability](features/02-capture.md#contract-02-5) |
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
| 03 §3.6 | [Interactions and Sequences](features/03-gate-and-pin.md#contract-03-3-6) |
| 03 §3.7 | [Database Schemas and Tables](DESIGN.md#contract-03-3-7) |
| 03 §3.8 | [Deployment Topology](DESIGN.md#contract-03-3-8) |
| 03 §4.1 | [The adopted predicate set (normative)](DESIGN.md#contract-03-4-1) |
| 03 §4.2 | [The Orders delta (normative)](features/03-gate-and-pin.md#contract-03-4-2) |
| 03 §4.3 | [The accepted order pin (normative)](DESIGN.md#contract-03-4-3) |
| 03 §4.4 | [The resolved total and TCV (normative)](DESIGN.md#contract-03-4-4) |
| 03 §4.5 | [What the order-time total excludes (normative)](DESIGN.md#contract-03-4-5) |
| 03 §4.6 | [Preview (normative)](features/03-gate-and-pin.md#contract-03-4-6) |
| 03 §5 | [Traceability](features/03-gate-and-pin.md#contract-03-5) |
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
| 04 §3.6 | [Interactions and Sequences](features/04-versioning.md#contract-04-3-6) |
| 04 §3.7 | [Database Schemas and Tables](DESIGN.md#contract-04-3-7) |
| 04 §3.8 | [Deployment Topology](DESIGN.md#contract-04-3-8) |
| 04 §4.1 | [Admissibility (normative)](features/04-versioning.md#contract-04-4-1) |
| 04 §4.2 | [Carry forward and re-resolve (normative)](features/04-versioning.md#contract-04-4-2) |
| 04 §4.3 | [Re-approval is a two-step seam interaction (normative)](features/04-versioning.md#contract-04-4-3) |
| 04 §4.4 | [Stale results (normative)](features/04-versioning.md#contract-04-4-4) |
| 04 §4.5 | [Version history (normative)](features/04-versioning.md#contract-04-4-5) |
| 04 §4.6 | [Administrative edits are last-write-wins (normative)](features/04-versioning.md#contract-04-4-6) |
| 04 §5 | [Traceability](features/04-versioning.md#contract-04-5) |
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
| 05 §3.6 | [Interactions and Sequences](features/05-preconditions.md#contract-05-3-6) |
| 05 §3.7 | [Database Schemas and Tables](DESIGN.md#contract-05-3-7) |
| 05 §3.8 | [Deployment Topology](DESIGN.md#contract-05-3-8) |
| 05 §4.1 | [The acceptance-required election (normative)](features/05-preconditions.md#contract-05-4-1) |
| 05 §4.2 | [Acceptance on the two paths (normative)](features/05-preconditions.md#contract-05-4-2) |
| 05 §4.3 | [Authorization as a guard input (normative)](features/05-preconditions.md#contract-05-4-3) |
| 05 §4.4 | [What this design cannot express (normative statement of limitation)](DESIGN.md#contract-05-4-4) |
| 05 §5 | [Traceability](features/05-preconditions.md#contract-05-5) |
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
| 06 §3.6 | [Interactions and Sequences](features/06-workflow-seam.md#contract-06-3-6) |
| 06 §3.7 | [Database Schemas and Tables](DESIGN.md#contract-06-3-7) |
| 06 §3.8 | [Deployment Topology](DESIGN.md#contract-06-3-8) |
| 06 §4.1 | [The five operations are ordinary transitions (normative)](DESIGN.md#contract-06-4-1) |
| 06 §4.2 | [Verdicts and the deciding authority (normative)](DESIGN.md#contract-06-4-2) |
| 06 §4.3 | [Begin fulfillment and the spawn signal (normative)](features/06-workflow-seam.md#contract-06-4-3) |
| 06 §4.4 | [Acknowledgement (normative)](features/06-workflow-seam.md#contract-06-4-4) |
| 06 §4.5 | [The per-line projection is not a state machine (normative)](DESIGN.md#contract-06-4-5) |
| 06 §4.6 | [The upstream asks this slice depends on](DESIGN.md#contract-06-4-6) |
| 06 §5 | [Traceability](features/06-workflow-seam.md#contract-06-5) |
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
| 07 §3.6 | [Interactions and Sequences](features/07-hold-and-expiry.md#contract-07-3-6) |
| 07 §3.7 | [Database Schemas and Tables](DESIGN.md#contract-07-3-7) |
| 07 §3.8 | [Deployment Topology](DESIGN.md#contract-07-3-8) |
| 07 §4.1 | [Hold and resume (normative)](features/07-hold-and-expiry.md#contract-07-4-1) |
| 07 §4.2 | [Bounded lifetime (normative)](features/07-hold-and-expiry.md#contract-07-4-2) |
| 07 §4.3 | [The `in_fulfillment` exemption (normative)](features/07-hold-and-expiry.md#contract-07-4-3) |
| 07 §4.4 | [Draft abandonment (normative)](features/07-hold-and-expiry.md#contract-07-4-4) |
| 07 §4.5 | [Policy values (open)](DESIGN.md#contract-07-4-5) |
| 07 §4.6 | [The ordinary cancel operation (normative)](features/07-hold-and-expiry.md#contract-07-4-6) |
| 07 §5 | [Traceability](features/07-hold-and-expiry.md#contract-07-5) |
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
| 08 §3.6 | [Interactions and Sequences](features/08-read-and-authz.md#contract-08-3-6) |
| 08 §3.7 | [Database Schemas and Tables](DESIGN.md#contract-08-3-7) |
| 08 §3.8 | [Deployment Topology](DESIGN.md#contract-08-3-8) |
| 08 §4.1 | [The read projection (normative)](DESIGN.md#contract-08-4-1) |
| 08 §4.2 | [What a read exposes (normative)](DESIGN.md#contract-08-4-2) |
| 08 §4.3 | [The permission model (normative)](DESIGN.md#contract-08-4-3) |
| 08 §4.4 | [Delegation proof (normative)](DESIGN.md#contract-08-4-4) |
| 08 §4.5 | [Policy values](DESIGN.md#contract-08-4-5) |
| 08 §5 | [Traceability](features/08-read-and-authz.md#contract-08-5) |


**D-188 implementation prerequisite:** Foundation delivers engine-owned durable attempts, allocation high-water, execution-generation idempotency/fencing, bounded recovery and sparse-history schemas before Gate-and-pin issues Pricing commands. Capture supplies immutable draft snapshots; Gate/Versioning persist exact receipt links only in the final commercial transaction. Workflow/Reads consume exact versions and explicit predecessors. The [D-188 failure matrix](DESIGN.md#contract-01-commercial-attempt) is a required completion gate; no runtime delivery is asserted. R03 follows D-189; R04 follows D-190; D-191/R06/R12 remain separate release prerequisites.


**D-189 implementation prerequisite:** Pricing owns the proposed assessment SDK/provider,
shared commercial evaluator and seller-scoped assessment authorization. Gate-and-pin consumes
that capability through the Orders port alongside the D-187 typed reads and composes one
assessment for Preview/submit/amendment. Preview does not depend on or invoke D-188 attempts;
only committing commands use them. Delivery requires the [D-189 conformance scenarios](DESIGN.md#contract-03-nonbinding-assessment),
including no commercial artifacts, complete independent diagnostics, missing-term TCV withholding,
least-privilege access and authoritative rejection after a changed observation. The upstream
assessment remains missing, and D-192 encoding/downstream mapping and the D-190/D-191 activation/hold/deadline implementation stay open.

### R04 implementation work (D-190; not yet delivered)

- [ ] Gate/Versioning persist selected committed receipt identities/digests through D-188; no hold at submit.
- [ ] Read/Authorization exposes exact version receipt links under finite authorized order scope.
- [ ] Subscriptions owns immutable hold input, idempotent retry, fresh eligibility and receiver admission fencing; Workflow passes exact committed version/attempt.
- [ ] Workflow implements explicit receiver reason mappings, preserving market-divergence and separating expiry/closure from transport/authorization failures and delivers compensation/unknown-outcome recovery.
- [ ] Deliver [D-190 cross-service conformance](DESIGN.md#contract-03-accepted-price-activation); D-191 validity/policy integration, D-192 snapshot mapping implementation and R12 concrete atomic admission/recovery block production activation.

### R05 implementation work (D-191; not yet delivered)

- [ ] Pricing: deliver the [seller-policy discovery/resolution contract](DESIGN.md#contract-03-commercial-deadline), immutable per-seller policy versions and authorized read grants. The current deployment-wide default does not fulfill seller-specific policy.
- [ ] Orders adapter/Gate/Versioning: stage assessment without an issued deadline, freeze discovered policy versions in D-188 attempts, then validate returned receipts and copy per-line `hold_until` verbatim before commit. Remove the local interval/end-date calculation.
- [ ] Reads/Preview: expose committed per-line deadline provenance; Preview carries no issued deadline and marks any optional forecast nonbinding/unavailable as appropriate.
- [ ] Workflow/Subscriptions: compare each planned activation against selected receipt windows, use fresh Pricing eligibility and retain R12 admission/recovery as a separate prerequisite; never refresh validity on replay or hold/resume.
- [ ] Deliver the D-191 boundary, policy-race, seller authorization, mixed-deadline, Preview and unchanged-replay conformance scenarios before claiming completion.


### R06 implementation work (D-192; not yet delivered)

- [ ] Foundation/Gate/Versioning: implement Orders schema-2 lossless complete-receipt encoding, original digest preservation, immutable query/link validation and final-commit selection under D-188. Explicitly retire the listed legacy provenance/matrix fields; retain required business-fact checks.
- [ ] Read/Authorization: expose exact committed receipt evidence only through the authorized order/version; separate optional current display/diagnostic projections from accepted commercial facts.
- [ ] Subscriptions/Rating: deliver the [upstream adoption requirement](UPSTREAM_REQS.md#r06-downstream-adoption-requirement-d-192-not-delivered), first-period materialization and broader Rating-owned snapshot composition; no receipt-only claim of a complete billable snapshot.
- [ ] Deliver [D-192 conformance](DESIGN.md#contract-03-frozen-commercial-snapshot), including complete BillingTerms, exact serialization, descriptor/meter drift, hold mismatch, missing real inputs and replay. Existing Pricing test source is evidence, not integration completion; R12 remains open.


### R07 implementation dependency — billing-terms producer (D-193)

- [ ] `p1` — Subscriptions delivers the proposed SafeRead SDK/provider and authorized immutable
  policy resolution in `…-upreq-pre-subscription-billing-terms`; existing Pricing validation is
  not a terms producer. Policy source/precedence and grants must be agreed upstream.
- [ ] `p1` — Capture retains calendar-unit/rolling intent; Gate adapts exact invoice-period Term
  and BillingTerms, validates canonical digest and freezes query inputs through D-188/D-192.
- [ ] `p1` — Preview preserves missing-term diagnostics/TCV withholding without commercial writes;
  acceptance requires complete terms. Prove supported anchors/cycles/models, retry stability and
  delayed activation/period geometry with actual Subscriptions/Pricing/Rating providers.

The [D-193 contract and conformance](DESIGN.md#contract-03-billing-terms-resolution) is a production
acceptance dependency, not a claim that the docs-only Subscriptions gear now has a resolver.


### R08 implementation dependency — service authentication and grants (D-194)

- [ ] Platform AuthN/AuthZ provisions separate seller-scoped Orders/Subscriptions ordinary service
  principals; wire authenticated contexts rather than handcrafted system subjects. Track
  `cpt-cf-bss-orders-lifecycle-upreq-commercial-service-provisioning` as a phase-1 release gate.
- [ ] Apply the [D-194 call matrix](DESIGN.md#contract-08-commercial-service-authorization) to
  adapter/provider integration: Orders acceptance create/read, Subscriptions hold/read, required
  catalog/Products reads; no implicit reference writes or Orders hold grant.
- [ ] Coordinate read permissions with delivery of D-189 assessment, D-191 policy and D-193 terms.
  Preview requires no acceptance-create/hold authority. Keep original buyer/proposed-payer checks
  before foreign-party calls and final commit.
- [ ] Persist authenticated commercial principal identity with D-188 attempts; validate stable
  identity through refresh/rotation, changed-principal reconciliation, revocation and stale workers.
- [ ] Pass real-provider positive/negative grant tests, seller isolation, unauthorized payer use,
  no impersonation and no leakage on denial/outage. Existing code paths are not deployed grants.

### R09 implementation dependency — diagnostic contract and mapping (D-195)

- [ ] Deliver the [D-195 Pricing-owned profile/results and Orders mapping registry](DESIGN.md#contract-03-diagnostic-mapping)
  alongside D-189 assessment; verify authoritative roster and independent expected coverage.
- [ ] Implement lossless producer subresults, mapping/profile/applicability metadata, tagged nullable
  selection identity and schema constraints; preserve stable Orders reasons and public allowlists.
- [ ] Apply blocking-only primary-error selection and optional-context TCV withholding consistently
  across assessment, persistence, REST and exact idempotent replay.
- [ ] Pass complete/malformed/partial coverage, multi-failure, many-to-one mapping, null/default
  collision, redaction, bounded deadline and registry-upgrade replay tests. Runtime work remains open.

### R10 implementation dependency — revision reference lifetime (D-196)

- [ ] Agree and deliver the [Pricing-coordinated closure/drain contract](DESIGN.md#contract-03-revision-reference-protection)
  with Orders and Subscriptions; keep existing revision references until this protocol is proven.
- [ ] Add authorized, seller/revision-scoped usage reporting and durable admission-generation
  acknowledgements. Include potentially committable D-188 attempts, nonterminal committed versions
  and durable receiver handoff; separate commercial holders from historical/orphan evidence.
- [ ] Coordinate final commit/recovery checks with the closure generation; persist release work in
  Pricing's durable recovery protocol. No Orders Products-reference writer or direct DB read is added.
- [ ] Prove concurrent submit/amendment versus closure, unknown command outcomes, process restart,
  stale/denied reports, activation handoff and duplicate release. Track R12 activation fencing as a
  separate dependency. No automated revision release may ship based solely on a zero usage count.

### R11 implementation dependency — Rating exact-binding evaluation (D-197)

- [ ] Publish and implement the [D-197 Rating SDK/provider](DESIGN.md#contract-03-rating-purchase-evaluation), supported profile and authorized grants; no Rating runtime exists yet.
- [ ] Integrate exact selected bindings and D-193 terms, frozen D-188 input/results and receipt equality; Orders performs no money arithmetic.
- [ ] Implement nullable uncommitted usage amounts, one order TCV carrier and labelled finite/rolling/mixed basis in storage and reads.
- [ ] Run the contract's real-provider boundary/replay/authorization/Preview fixtures; development doubles cannot satisfy production readiness. Tax remains separate.


**D-198 production gate:** Foundation implements staged receiver controls and recovery; Workflow integrates complete receiver pause/revoke inventory evidence; Subscriptions implements capacity reservation at async intent admission and shared-generation confirmation fencing. Hold/ordinary terminal effective commits wait for the barrier, with the explicit D-182 unknown exception. Canonical resource-dimensional key adoption, SDKs/grants, all-writer capacity and failure/race conformance remain unchecked; local Orders doubles or passing Pricing hold tests do not satisfy this gate. [Normative protocol](DESIGN.md#contract-06-activation-admission).

### R13 implementation dependency — commercial authorities (D-199)

- [ ] Implement the [owner readiness matrix](DESIGN.md#contract-05-commercial-owner-readiness): payer profile/delegation, Contracts, Payments authorization and Workflow approval/fulfillment providers with explicit schemas, grants and freshness.
- [ ] Preserve Pricing receipt/customer consent/approval separation and D-175 adapter trust boundary; bind each relevant decision to current version.
- [ ] Test real-provider refusal/outage/stale-version behavior and durable pending payment recovery before enabling affected production paths; no guessed facts or mock-based readiness.

### R14 implementation dependency — integrate the existing event platform (D-200)

- [ ] Deliver the [D-200 integration checklist](DESIGN.md#contract-01-event-platform-integration)
  using existing EventBrokerApi/DbProducer/toolkit facilities, same-transaction enqueue and post-commit Wake.
- [ ] Register eleven event types/subject/topic, configure root identity/grants and routing/partitions,
  and verify runtime readiness. The broker implementation and initial cursor-retry behavior exist.
- [ ] Record deployed SDK evidence and empty-cache transient recovery regression; deliver supported
  producer dead-letter republication, shared operator tooling, runbook and monitoring evidence.
- [ ] Pass real-provider lost-response/restart/recovery/security tests and D-186 consumer conformance;
  broker acceptance does not establish downstream completion. All integration tasks remain open.
