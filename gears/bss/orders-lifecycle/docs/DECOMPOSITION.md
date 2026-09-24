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
- [4. Contract address index](#4-contract-address-index)

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
  - The order aggregate and its append-only version chain; the declarative state-machine table with its guards, terminal set, hold/resume mapping and expiry eligibility; guard evaluation and ordering; the idempotency registry and its non-success outcomes; the optimistic version check and its `version-conflict` refusal — the single registered name D-38 consolidated the `stale-version` variants into; the append-only transition audit; the typed event contract and the one-producer-message-per-event-declaring-transition rule; and the registry of machine-readable business reasons.

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
    - `cpt-cf-bss-orders-lifecycle-constraint-guard-input-ports`
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

- **Phase**: 0/1; [detailed design](DESIGN.md#contract-01-1-1).

- **Event contract**: `cpt-cf-bss-orders-lifecycle-contract-order-events`; eleven typed events and the existing event-less transition classes, with platform producer delivery acceptance.

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
  - The submit gate: the published pricing sellability predicates adopted by reference plus the Orders delta — tenant-axis validity, contract-active where referenced, purchase-quantity floor, order-market consistency against the payer's profile, reference resolution, single currency, overlap-rule uniqueness and the one-in-flight-order rule. Capture of the catalog price pin on every line and of the non-authoritative resolved total. The Preview operation, which creates no order or commercial artifact, persists bounded-retention gate outcomes and returns no approval verdict.

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
  - Hold and resume with the stored pre-hold state; the per-state TTL policy for `submitted`, `pending_approval`, `approved` and `on_hold`; the **resume cap** that stops a hold/resume cycle restarting the dwell without limit — its sibling, the amendment cap, is owned in [04 §4.1](features/04-versioning.md#contract-04-4-1) because its value is a commercial judgment; the coordinated expiry scheduler; and the transition-table exclusion of `in_fulfillment` and of holds taken from it, together with the handoff of those cases to the operational escalation owned by the sibling gear. It does **not** supply a fallback duration for a state whose TTL is unset — such a state is unbounded, disclosed in [07 §4.2](features/07-hold-and-expiry.md#contract-07-4-2) and routed as [`DECISIONS.md`](./DECISIONS.md) Q-27.

- **Out of scope**:
  - It does not pause entitlement or billing on already-activated subscriptions, does not extend a term, and does not void wave-1 subscription drafts. It never auto-terminals an order whose fulfillment may be in flight.

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
Pricing's current [`domain/sellability.rs`](../../pricing/pricing/src/domain/sellability.rs)
implements four predicate families: active-window coverage/horizon, committed catalog version,
availability dates and plan lifecycle. The GA/prepaid-execution flags and registry sellability
families remain `NotEvaluable`; the registry flag is owned by `products`. The older claim that
active-window coverage is unimplemented is superseded by Pricing's window-linkage implementation.

Implemented predicates are not yet a consumable Orders interface: the public
[`pricing-sdk/src/api.rs`](../../pricing/pricing-sdk/src/api.rs) exposes only `pin_frontier`.
Batched fixed-version predicates and pin composition still require public SDK publication.
Bundles additionally require frozen component/key composition and wiring of Pricing's component
conjunction into that SDK. Its current own-price path omits component evaluation; publishing
that path unchanged would not satisfy Orders' gate. The dedicated bundle requirement in
[UPSTREAM_REQS.md §2.2](UPSTREAM_REQS.md#22-rating--price-evaluation) remains open, with fail-closed bundle assessment until complete
pinned coverage is supplied.
`pin_frontier` also reads the caller's tenant rather than the seller's catalog, so the
explicit catalog-tenant reads (`cpt-cf-bss-orders-lifecycle-upreq-pricing-catalog-tenant-reads`, D-122) are a prerequisite
too. No Rating SDK crate exists (`cpt-cf-bss-orders-lifecycle-upreq-rating-evaluation`), Account Management exposes
`get_tenant` for tenant-axis validity but no payer commercial profile
(`cpt-cf-bss-orders-lifecycle-upreq-payer-commercial-profile`), and Contracts has no implementation for contract status and
party eligibility (`cpt-cf-bss-orders-lifecycle-upreq-contract-party-eligibility`); until they land the gate refuses
`evaluation-unavailable`, `identity-party-unavailable` and `contract-resolution-unavailable`.
Subscriptions' occupancy/cardinality read (`SUB-O5`) and activation enforcement are separate
open prerequisites. See [03 §2.2](DESIGN.md#contract-03-2-2) and `UPSTREAM_REQS.md` for owners and acceptance contracts.
Capture and engine development can proceed against doubles, subject to the authorization
integration prerequisite in [01 §3.6](features/01-foundation.md#contract-01-3-6); this is not a production-readiness claim. Product-visible
fail-closed behavior remains routed as `DECISIONS.md` Q-15. Runtime implementation, public
interface availability and deployed authorization/integration evidence must be tracked separately.

Phase 0/1 is the correctness core and is a prerequisite for everything else. Its event-producing
runtime is additionally blocked until the Event Broker implementation exists: [docs/GEARS.md](../../../../docs/GEARS.md)
currently records “SDK landed — impl crate TODO”. Startup must prepare all event types, resolve the
managed chained producer and start the toolkit outbox workers before readiness. Capture and local
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
| 03 §4.3 | [The catalog price pin (normative)](DESIGN.md#contract-03-4-3) |
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
