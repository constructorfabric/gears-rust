<!-- CONFLUENCE_TITLE: [BSS]: Orders Lifecycle — Buyer Acceptance and the Money Gate (Slice 5) -->
<!-- Related: ../DESIGN.md, ../PRD.md, ./README.md, ./01-foundation.md | Owners: BSS Orders team -->

# DESIGN — Buyer Acceptance and the Money Gate (Slice 5)


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
  - [4.1 The acceptance-required election (normative)](#41-the-acceptance-required-election-normative)
  - [4.2 Acceptance on the two paths (normative)](#42-acceptance-on-the-two-paths-normative)
  - [4.3 Authorization as a guard input (normative)](#43-authorization-as-a-guard-input-normative)
  - [4.4 What this design cannot express (normative statement of limitation)](#44-what-this-design-cannot-express-normative-statement-of-limitation)
- [5. Traceability](#5-traceability)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-design-preconditions`

## 1. Architecture Overview

### 1.1 Architectural Vision

This slice owns the two facts that must be true before an order may begin fulfillment: that the
customer agreed to the purchase, and that the payer can pay for it
([`../PRD.md`](../PRD.md) §6.1).

They are different kinds of fact and the design treats them differently. **Acceptance is
recorded**, once per immutable commercial version, as a first-class instant with its own transition and its own event — because in
a dispute over a partner-placed order it is the only evidence that the customer agreed at all.
**Authorization is consumed**, as a guard input supplied by the sibling gear at the moment of
begin-fulfillment — because it is a point-in-time risk answer owned by a capability that does
not exist yet, and storing it would imply an authority this gear does not have.

The governing rule for acceptance is that it **has no default value at any layer**. The line
model carries an acceptance *due date* with a cascade that fills it from the contract-effective
date, and that cascade must never be mistaken for assent. A due date says when agreement is
expected; the instant says it happened. Conflating them would turn a calendar field into
fabricated consent, which is the one failure mode this slice exists to prevent.

The money gate is deliberately thin, and its thinness is a stated limitation rather than an
omission. Only **provision-then-collect** is expressible: an authorization outcome read after
the order is `approved`, with at-sale money posted downstream when Subscriptions emits billable
facts at activation. A self-service card checkout inverts that ordering, and this design cannot
carry it — §4.4 says so explicitly rather than leaving a reader to discover it.

### 1.2 Architecture Drivers

#### Functional Drivers

| Requirement | Design Response |
|-------------|------------------|
| `cpt-cf-bss-orders-lifecycle-fr-order-acceptance` | Acceptance binds to the current immutable commercial version through a state-only transition on a submitted, non-terminal order, publishing `OrderAcceptanceRecorded`; one row per accepted version records the actor and requirement source. Never defaulted. |
| `cpt-cf-bss-orders-lifecycle-fr-order-payment-auth` | Authorization is a begin-fulfillment guard input, not a stored order fact: a three-valued outcome; only `authorized` and `failed` are expected on begin-fulfillment (a `pending` submission is refused defensively, §4.3, D-131); the seller tolerate-failure election is read at guard time and the resulting risk is flagged and audited. |
| `cpt-cf-bss-orders-lifecycle-fr-order-create` | On the self-service path, submit by the buyer *is* acceptance and is recorded as such in the submit commit — no separate field, no second call. A submit counts as the buyer's by §4.2's submit-request rule (no proof reference, submitter tenant = resource tenant, D-146), not by `sales_path`. |
| `cpt-cf-bss-orders-lifecycle-fr-orders-boundary-r3-provisioning` | Nothing here provisions or touches money. The slice supplies guard inputs and records one fact. |

#### NFR Allocation

| NFR ID | NFR Summary | Allocated To | Design Response | Verification Approach |
|--------|-------------|--------------|-----------------|----------------------|
| `cpt-cf-bss-orders-lifecycle-nfr-order-audit-completeness` | 100 % of transitions audited | Acceptance recorder | Recording is an engine transition, so the actor, instant and requirement source audit with it | Test asserting an acceptance record always has a paired audit entry naming its actor |
| `cpt-cf-bss-orders-lifecycle-nfr-order-transition-latency` | Commit p95 < 1 s | Acceptance recorder | Recording resolves the requirement source from already-stored data and makes no outbound call | Load test on the acceptance transition |
| `cpt-cf-bss-orders-lifecycle-nfr-order-idempotency` | Zero duplicate effects | Acceptance recorder | At most one acceptance row per order/version, enforced by the primary key; a replayed recording returns the stored outcome | Concurrency test firing duplicate recordings for one version and asserting one row |

#### Key ADRs

The seven gear ADRs govern this slice. One decision taken here is recorded in the register: **authorization is consumed as a guard
input and never stored as an order fact** ([`../DECISIONS.md`](../DECISIONS.md) D-78, §4.3),
whose alternative was a `payment_pending` order state.

### 1.3 Architecture Layers

Inherited from [`01-foundation`](./01-foundation.md) §1.3. Lifecycle has no authorization port;
the outcome arrives as a request input, so this slice adds no infrastructure of its own.

## 2. Principles and Constraints

### 2.1 Design Principles

#### The acceptance instant is never defaulted

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-principle-acceptance-never-defaulted`

No policy, cascade, migration or convenience path may write an acceptance instant that a party
did not perform. The line-level acceptance **due date** has a cascade; the instant has none.
Because the column exists only when a real instant was recorded, its presence is evidence and
its absence is a truthful answer.

#### Agreement and delegation are different facts

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-principle-agreement-not-delegation`

On the partner-placed path the order evidences the partner's **right to act** — carried by the
initiating actor and the delegation proof PDP policy requires at the engine's authorization pre-guard (D-111). It does
not evidence the customer's **agreement to the purchase**. The two are recorded separately
because a dispute distinguishes them, and a design that conflated them would offer a partner's
own authority as proof of their customer's consent.

#### Authorization is read, not owned

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-principle-authorization-read-not-owned`

The authorization outcome is a point-in-time answer from a capability this platform has not
specified. This slice consumes it as a guard input at begin-fulfillment and stores no
instrument, no token and no outcome as an order fact. Storing it would imply this gear could
answer "is the payer good" later, which it cannot.

### 2.2 Constraints

#### There is no `payment_pending` state

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-constraint-no-payment-pending-state`

A pending authorization leaves the order `approved` with begin-fulfillment simply not called. No
twelfth state is added. The consequence is real and is not hidden: an order awaiting
authorization and a healthy order awaiting the sibling gear's next step are indistinguishable
from the order document alone, and process visibility is the sibling gear's to provide.

#### A declined instrument exits by expiry, and only where the TTL is set

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-constraint-declined-instrument-exit`

Where authorization fails and the seller has not elected tolerate-failure, Lifecycle refuses
begin-fulfillment and the order remains `approved` until its TTL elapses. Two qualifications, and both are
this constraint's real content rather than footnotes. The `approved` TTL is a **Product-owned open
question with no code default**, so **while it is unset this order has no automatic exit at all** —
only a caller-driven cancel retires it ([`07-hold-and-expiry`](./07-hold-and-expiry.md) §4.5). And
where it *is* set, the **two re-entry caps** of `07 §4.2` are what stop a hold/resume or amendment
cycle restarting dwell without limit. The configured pre-fulfillment dwell budgets sum to at
most `74 × T_max` under `07 §4.2`'s assumptions, plus scheduler delay; this is not an unconditional
calendar exit bound (`../DECISIONS.md` D-90). There is no re-authorize
operation, no payment failure event and no order-visible outcome, because there is no Payments
capability to supply one. This is routed as [`../DECISIONS.md`](../DECISIONS.md) Q-08 — the PRD
carries **no** §15 row for it — and stated as a designed limitation rather
than deferred silently: the order expires, and the buyer learns nothing from the order document.

#### Payment collection is out of scope entirely

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-lifecycle-constraint-no-payment-collection`

Capture, settlement, strong-customer-authentication challenges and their asynchronous return,
retry with an alternative instrument, refunds, chargebacks and provider webhooks are all outside
this gear and outside the sibling gear. The slice consumes an **authorization** outcome and
nothing further. No cardholder data is handled anywhere, so payment-card compliance is not
applicable to this gear.

## 3. Technical Architecture

### 3.1 Domain Model

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-entity-acceptance-record`

The customer-acceptance instant as a recorded fact: when it happened, which actor recorded it,
and whether the requirement came from a referenced contract, a seller election, the platform
default, or was volunteered where none was required (D-107). At
most one per immutable commercial version. Only acceptance of the current version gates begin-fulfillment where acceptance is required; its
absence is never inferred to be satisfaction.

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-lifecycle-entity-authorization-outcome`

A **transient** guard input, not a persisted entity: the authorization verdict supplied by the
sibling gear at begin-fulfillment — a three-valued outcome (authorized, pending, failed); only
`authorized` and `failed` are expected on begin-fulfillment, and `pending` is refused
defensively (§4.3, D-131). Its only durable trace is
the risk flag and the audit entry written when a tolerate-failure election admits a failed
authorization.

**Relationships**:
- `Order root` → `Acceptance record`: zero-to-many across versions, at most one per version. Present only where a real instant was recorded.
- `Acceptance record` → `Order transition`: one-to-one; the recording is itself an audited transition.
- `Authorization outcome` → `Order transition`: contributes to the begin-fulfillment audit entry, including the risk flag where one applies.

### 3.2 Component Model

This slice realises `cpt-cf-bss-orders-lifecycle-component-preconditions`
([`../DESIGN.md`](../DESIGN.md) §3.2) as two internal parts.

#### Acceptance recorder

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-component-preconditions-acceptance`

##### Why this component exists

Without a recorded instant there is nothing showing the customer agreed to a partner-placed
purchase, and the order's whole evidentiary value collapses at the point it matters most.

##### Responsibility scope

Resolution of the acceptance-required election by the §4.1 precedence (contract, then seller
election, then platform election, then the safe fallback); recording the instant with its actor; the self-service rule that makes buyer submit
constitute acceptance; and the at-most-one-per-order/version invariant.

##### Responsibility boundaries

It writes no default value under any policy. It does not decide whether fulfillment may begin —
it supplies one of two guard inputs.

##### Related components (by ID)

- `cpt-cf-bss-orders-lifecycle-component-transition-orchestrator` — depends on
- `cpt-cf-bss-orders-lifecycle-component-preconditions-money-gate` — shares model with

#### Money gate

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-component-preconditions-money-gate`

##### Why this component exists

Without a money check before provisioning, a non-paying tenant receives resources and the
failure surfaces later as dunning over consumed capacity — the most expensive compensation path
the platform has.

##### Responsibility scope

The begin-fulfillment guard: the authorization outcome as input, the three-way distinction
between authorized, pending and failed, the seller tolerate-failure election read at guard time,
and the risk flag recorded when a failed authorization is tolerated.

##### Responsibility boundaries

It holds no payment mechanism, no instrument and no token, performs no credit scoring, and
stores no authorization outcome as an order fact.

##### Related components (by ID)

- `cpt-cf-bss-orders-lifecycle-component-workflow-seam` — shares model with

### 3.3 API Contracts

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-interface-preconditions-ops`

- **Requirement**: `cpt-cf-bss-orders-lifecycle-interface-order-ops`
- **Technology**: REST/OpenAPI via `OperationBuilder`; RFC 9457 problems

| Method | Path | Description | Stability |
|--------|------|-------------|-----------|
| `POST` | `/bss-orders-lifecycle/v1/orders/{orderId}/acceptance` | Record acceptance of `expected_version`, which must be the current immutable commercial version | unstable |
| `GET` | `/bss-orders-lifecycle/v1/orders/{orderId}/acceptance` | Read current `version`, its acceptance or absence, and prior version-bound records; executes `08 §3.6`'s common read wrapper under `order × read`, access-logged | unstable |

The begin-fulfillment operation itself is owned by
[`06-workflow-seam`](./06-workflow-seam.md); this slice contributes its two guards.

**Reasons contributed to the registry**: acceptance-already-recorded,
acceptance-recording-party-barred (§4.2, D-130),
acceptance-required-not-recorded,
authorization-pending, authorization-failed, authorization-failed-tolerated (an admission
carrying a risk flag, not a refusal), and **acceptance-requirement-unevaluable** — the
unevaluable reason for this slice's guard inputs, required because `01 §3.6` *Attempt Transition* step 3.1.2 settles an
unresolvable input with its guard's registered unevaluable reason (D-08), and this slice resolves the
acceptance requirement from the contracts port, whose unreachability would otherwise have no
registered name. A terminal order needs no slice reason: transition row 25
admits the acceptance instant **from any non-terminal state except draft**, so a draft or terminal order has no row and
the engine's own `not-admissible` refuses it — matching the treatment
[`07-hold-and-expiry`](./07-hold-and-expiry.md) §3.3 gives an ineligible expiry
([`../DECISIONS.md`](../DECISIONS.md) D-38).

### 3.4 Internal Dependencies

| Dependency Gear | Interface Used | Purpose |
|-------------------|----------------|----------|
| `toolkit-db` | Runtime-scoped access, via the engine | The acceptance record, inside the transition transaction |

### 3.5 External Dependencies

| Dependency Gear | Interface Used | Purpose |
|-------------------|---------------|---------|
| `contracts` | SDK client — **unexposed today** — via the contract-resolution port of `03 §3.3`, whose output carries `acceptance_required` where a contract is referenced (D-132) | The acceptance-required declaration where a contract is referenced, read live at each guard (*Record Acceptance* step 2, *Evaluate Begin-Fulfillment Preconditions* step 1) and never snapshotted; seller or platform election otherwise (§4.1). Raised as `cpt-cf-bss-orders-lifecycle-upreq-contract-acceptance-declaration`; until the SDK exists a contract-referenced order resolves `acceptance-requirement-unevaluable` (fail closed), never the §4.1 safe fallback |
| Payments | Reached only through `orders-workflow` | The authorization outcome. No gear and no specification exists in this repository; the outcome arrives as a guard input |

**Dependency Rules** (per project conventions):
- No circular dependencies
- Always use SDK modules for inter-gear communication
- No cross-category sideways deps except through contracts
- Only integration/adapter gears talk to external systems
- `SecurityContext` must be propagated across all in-process calls

### 3.6 Interactions and Sequences

#### Record the acceptance instant

**ID**: `cpt-cf-bss-orders-lifecycle-seq-record-acceptance`

**Use cases**: `cpt-cf-bss-orders-lifecycle-usecase-order-new-acquisition`

**Actors**: `cpt-cf-bss-orders-lifecycle-actor-orders-partner-admin`, `cpt-cf-bss-orders-lifecycle-actor-orders-direct-customer`

**Algorithm: Record Acceptance**

Input: order_id, expected_version, recording_actor, security_context, idempotency_key
Output: recorded, or a registered refusal

1. [ ] - `p1` - Declare guards the engine evaluates and audits: submitted non-terminal state (never `draft`), expected_version current (engine `version-conflict`), not-already-recorded for that version (step 5), and recording-party (step 4, `acceptance-recording-party-barred`) - `inst-ra-declare-guards`
2. [ ] - `p1` - Resolve whether acceptance is required by the §4.1 precedence: the referenced contract's declaration where a contract is referenced (the `acceptance_required` field of the `03 §3.3` contract-resolution port, called live outside the gate, D-132), else the seller-scope election, else the platform-scope election, else the safe fallback (required). If contract/policy resolution is unavailable, contribute acceptance-requirement-unevaluable to the engine's ordinary input-failure branch; do not interpret outage as an unset safe fallback - `inst-ra-resolve-requirement`
3. [ ] - `p1` - Permit recording on both sales paths, including renewed acceptance after amendment; resolve `requirement_source` to `contract`, `seller`, `platform_default` or `volunteered` (§4.1) — `seller` where a seller-scope election decided it, `platform_default` where the platform-scope election or the unset fallback did - `inst-ra-resolve-path-admissibility`
4. [ ] - `p1` - Resolve recording-party from stored facts only (§4.2, D-130, D-146): take the role versions — version 1 (the creator), the submitted version (the submitter) and `expected_version` (its amender, where that version was appended by an amendment). The guard refuses with `acceptance-recording-party-barred` when the trusted SecurityContext actor equals the `orders_order_version.actor` of a role version and either `orders_order.sales_path = partner_placed` (`01 §3.7`, D-140) or that role version's `orders_order_version.actor_tenant_id` differs from `orders_order.resource_tenant_id`. The bar is applied per role version, so a buyer who created its own order is not barred merely because a delegated partner submitted it. Otherwise the guard passes; `sales_path = self_service` alone never passes it (D-146). `resourceTenantId` membership and the `acceptance × record` grant are not re-checked here: they stay with the engine's PDP pre-guard (`operation-not-permitted-for-actor`, or `order-not-found` per D-114), which cannot see stored version actors (D-111) - `inst-ra-resolve-recording-party`
5. [ ] - `p1` - Resolve not-already-recorded by reading `(order_id, expected_version)`; an existing row for this version refuses with `acceptance-already-recorded`, while older rows do not block recording. Same-key replays use the engine's stored response - `inst-ra-resolve-already-recorded`
6. [ ] - `p1` - Contribute the engine's server transition timestamp, `accepted_version = expected_version`, recording actor from trusted SecurityContext, recording_path copied from the immutable `orders_order.sales_path` (D-106, D-140) and requirement source to the engine transaction; the final version check prevents a racing amendment from receiving assent intended for its predecessor - `inst-ra-record-instant`
7. [ ] - `p1` - Request the state-only acceptance transition so the engine audits it and publishes OrderAcceptanceRecorded - `inst-ra-request-transition`
8. [ ] - `p1` - **RETURN** recorded - `inst-ra-return-recorded`

**Description**: The instant is the engine's pre-write server timestamp committed with the transition, not a prediction of physical commit time or a caller-supplied value, because a
caller-supplied instant is a claim rather than an observation. The event is what lets the
sibling gear re-evaluate begin-fulfillment eligibility without polling.

#### Self-service submit constitutes acceptance

**ID**: `cpt-cf-bss-orders-lifecycle-seq-self-service-acceptance`

**Use cases**: `cpt-cf-bss-orders-lifecycle-usecase-order-new-acquisition`

**Actors**: `cpt-cf-bss-orders-lifecycle-actor-orders-direct-customer`

```mermaid
sequenceDiagram
    participant C as Direct Customer
    participant L as Orders Lifecycle
    C ->> L: submit (buyer is the resource tenant)
    L ->> L: gate passes; record acceptance in the same commit
    L -->> C: submitted, acceptance recorded
    Note over L: one commit, two facts, ONE event -<br/>OrderSubmitted carries the acceptance instant
```

**Description**: On the self-service path the submitting party *is* the accepting party, so a
second call would ask the buyer to agree to something they just bought. The recording is a
contribution to the submit commit and audits with it, and the instant travels in the
`OrderSubmitted` payload — a single event, because a single transition emits a single event.
The submit contribution is written only under §4.2's submit-request rule — the allowed submit
carried no delegation proof reference and the submitter's subject tenant equals the order's
`resourceTenantId` (D-146) — never on `sales_path` alone; otherwise the submit writes no acceptance.
When written it records `accepted_version` on the newly appended version, `recording_path`
= `self_service` (by construction: the rule admits only a direct own-tenant submit), trusted submitting actor, the engine's pre-write server transition timestamp `t`
(`01 §3.6` *Attempt Transition*) and resolved requirement_source.
Separate recording copies `orders_order.sales_path` (written once at create, D-106 — `partner_placed` iff the allowed create carried a delegation proof reference, `01 §3.7`, D-140), never infers it from the current actor.

#### Begin-fulfillment guard evaluation

**ID**: `cpt-cf-bss-orders-lifecycle-seq-begin-fulfillment-guards`

**Use cases**: `cpt-cf-bss-orders-lifecycle-usecase-order-fulfillment-complete`

**Actors**: `cpt-cf-bss-orders-lifecycle-actor-orders-workflow`, `cpt-cf-bss-orders-lifecycle-actor-orders-contracts`

**Algorithm: Evaluate Begin-Fulfillment Preconditions**

Input: order_id, expected_version, authorization_outcome, security_context
Output: admit, admit-with-risk-flag, or a registered refusal

1. [ ] - `p1` - Resolve acceptance and tolerance policies before the transaction, reading a referenced contract's `acceptance_required` live through the `03 §3.3` contract-resolution port (D-132); unavailable contract/policy inputs contribute acceptance-requirement-unevaluable through the engine; only an actual unset election uses its safe fallback - `inst-bg-resolve-requirement`
2. [ ] - `p1` - **IF** required **AND** no acceptance row exists for `(order_id, current_version)`: - `inst-bg-if-acceptance-missing`
   1. [ ] - `p1` - **RETURN** acceptance-required-not-recorded refusal - `inst-bg-return-acceptance-missing`
3. [ ] - `p1` - **MATCH** the authorization outcome: - `inst-bg-match-authorization`
   1. [ ] - `p1` - **WHEN** authorized: **RETURN** admit - `inst-bg-when-authorized`
   2. [ ] - `p1` - **WHEN** pending: **RETURN** authorization-pending refusal (the order stays `approved`); a defensive fail-closed branch, not a protocol step, since Workflow submits only conclusive outcomes (§4.3, D-131) - `inst-bg-when-pending`
   3. [ ] - `p1` - **WHEN** failed: - `inst-bg-when-failed`
      1. [ ] - `p1` - Read the seller tolerate-failure election - `inst-bg-read-tolerate-election`
      2. [ ] - `p1` - **IF** tolerate-failure is not elected: **RETURN** authorization-failed refusal - `inst-bg-if-not-tolerated`
      3. [ ] - `p1` - **RETURN** admit-with-risk-flag, to be recorded on the order and audited - `inst-bg-return-tolerated`

**Description**: Pending and failed are genuinely different answers and collapsing them would
either stall silently or provision a non-paying tenant. Neither refusal changes order state —
the order stays `approved`, and the sibling gear retries or escalates.

### 3.7 Database Schemas and Tables

This slice owns `orders_acceptance` (`cpt-cf-bss-orders-lifecycle-dbtable-acceptance`),
specified normatively in [`01-foundation`](./01-foundation.md) §3.7, and introduces
`orders_policy_election` below. Three additions to the engine-owned tables belong here:

- The primary key is `(order_id, accepted_version)`, with a foreign key to `orders_order_version(order_id, version)`, enforcing **at most one acceptance per immutable commercial version**. Rows are append-only; amendment retains previous rows and never copies acceptance to the new version.
- `accepted_at` is **NOT NULL with no default at any layer** — no column default, no application default, no backfill. The row exists only because an instant was recorded.
- `requirement_source` records whether the requirement came from a contract (`contract`), a seller-scope election (`seller`), the platform-scope election or its unset fallback (`platform_default`), or was not required at all (`volunteered`), so a later change to an election does not rewrite history about why acceptance was demanded (§4.1, D-107).

The **risk flag** for a tolerated authorization failure is
`orders_order.authorization_failure_tolerated_at`, specified in
[`01-foundation`](./01-foundation.md) §3.7, written by the begin-fulfillment transition and never
cleared, since it records a decision taken at a moment rather than a current condition.

#### Table: orders_policy_election

**ID**: `cpt-cf-bss-orders-lifecycle-dbtable-policy-election`

**Schema**: `election` enum (`tolerate_authorization_failure`, `acceptance_required`), `scope`
enum (`platform`, `seller`), `scope_id` (null for platform scope), `elected`, `elected_by`,
`elected_at`.

**PK**: (election, scope, scope_id)

**Constraints**: `NULLS NOT DISTINCT` on the key so a second platform row for one election is
impossible; `scope_id` NOT NULL where `scope = 'seller'` and NULL where `scope = 'platform'`.
Mutable only by deployment promotion; a seller's election is requested through platform
operations. `elected_by`/`elected_at` record the promotion's change identity and instant (D-133).
No Orders endpoint writes this table, and no PDP action governs it.

**Additional info**: seller scope overrides platform scope; for `acceptance_required` a referenced
contract's declaration overrides both (§4.1). An election with no row is read as its
safe value (tolerate-failure not elected; acceptance required).
This is the only table this slice introduces; the delivery path is the same policy channel that
carries `orders_state_ttl_policy`, promoted through environments with the deployment rather than
edited at runtime ([`../DESIGN.md`](../DESIGN.md) §3.8).

### 3.8 Deployment Topology

Inherited from [`01-foundation`](./01-foundation.md) §3.8. No background worker.

**Observability owned here**: authorization-outcome distribution across authorized, pending and
failed, kept as three series because collapsing them hides the declined-instrument population
§4.4 says has no exit but expiry; the count of admissions carrying the **tolerated-failure risk
flag**, which is a commercial risk register and not an error rate; acceptance-recording latency
against the acceptance-due date; and recording-party refusals (`acceptance-recording-party-barred`,
D-130), since those are the control preventing a placing party supplying its own customer's
consent. Alerts fire on the tolerated-failure count crossing its threshold, on any sustained
`acceptance-recording-party-barred` rate
(a security signal), and on a sustained `acceptance-requirement-unevaluable` rate, which fails
begin fulfillment closed on a contract/policy input this slice resolves. Unavailability of the
authorization dependency itself is not observable here — Lifecycle holds no authorization port —
and is alerted by Workflow, which owns that call and its retry budget.

## 4. Additional Context

### 4.1 The acceptance-required election (normative)

Whether acceptance is required **MUST** be resolved by exactly one precedence, highest first
(D-107):

1. the referenced contract's declaration, where a contract is referenced — `requirement_source = contract`;
2. the seller-scope `acceptance_required` election (§3.7) — `requirement_source = seller`;
3. the platform-scope `acceptance_required` election — `requirement_source = platform_default`;
4. the safe fallback, where no row exists at either scope: acceptance **required** —
   `requirement_source = platform_default`.

Steps 3 and 4 record the same `requirement_source = platform_default`: the acceptance row does
not distinguish an explicit platform-scope row from the unset fallback, and no other record does.

The resolved source **MUST** be stored on the acceptance row, so a later change to a contract,
a seller election or the platform election does not retroactively change the recorded reason.
An unavailable contract or policy input is not an unset election and never reaches step 4.

Where acceptance is required, the order **MUST NOT** enter `in_fulfillment` until the instant is
recorded for the current immutable commercial version. Where it is **not** required, a recording attempt on either sales path
**MUST** still be admitted, and `requirement_source` **MUST** record it as `volunteered` rather
than `contract`, `seller` or `platform_default` — so the row is never mistaken for evidence that policy
demanded it.

The previous rule refused the recording outright, on the reasoning that an acceptance row on an
order that never needed one is misleading evidence. That reasoning protects evidentiary hygiene
and defeats the requirement it serves: PRD §6.1's "a customer-acceptance instant **MUST** be
recordable as a first-class fact" is unconditional, and the required flag governs only whether
fulfilment waits for it. On an uncontracted or platform-default partner-placed order — the common
case — a genuine customer agreement could not be recorded at all, which is precisely the dispute
scenario the requirement exists for. Provenance on the row solves the hygiene problem without
withholding the fact ([`../DECISIONS.md`](../DECISIONS.md) D-71).

### 4.2 Acceptance on the two paths (normative)

**Which submit is a self-service submit (normative, D-146).** The automatic acceptance at submit
is keyed on facts of the submit request itself, never on `orders_order.sales_path`: it is written
**only** when (1) the submit request the engine's authorization allowed carried **no** delegation
proof reference, and (2) the submitting principal's trusted `SecurityContext.subject_tenant_id`
equals the order's `resourceTenantId`, frozen at submit (`02 §4.3`). Otherwise the submit writes no
acceptance, and acceptance, where required, **MUST** be recorded separately through §3.6 *Record
Acceptance*, subject to its recording-party bar. The automatic record's `recording_path` is
`self_service` by construction. `sales_path` cannot carry this rule: it is fixed at create, while
delegation can begin after it — a partner creates in its own tenant without a proof, edits
`resourceTenantId` in draft and submits with a proof, or a delegated partner submits a buyer's
draft — and on either path a `self_service` order's submit would otherwise record the partner's
submit as the customer's consent (D-31, D-130).

**Residual consequence of the proof-reference proxy (D-146).** Until the PDP names the proof it
accepted (`../UPSTREAM_REQS.md` `…-upreq-pdp-policy-integration` item 4), any supplied proof
reference counts. A self-service client that sends a proof reference on its own-tenant create is
recorded `partner_placed`, and one that sends it on its own-tenant submit gets no automatic
acceptance; its buyer must then record acceptance through a permitted recording party, and a
buyer who created or submitted the order is barred by step 4 where it is `partner_placed`. The
remedy is not to send a proof on an own-tenant request: clients **SHOULD NOT**. The dead-end
risk where no permitted party can reach an acceptance surface is tracked by
[`../DECISIONS.md`](../DECISIONS.md) Q-30.

On the **self-service** path, submit by the buyer **constitutes** acceptance and **MUST** be
recorded as such within the submit commit — as a **contribution to the submit transition**, not as
a second transition. Only `OrderSubmitted` is published, carrying the acceptance instant in its
payload together with `accepted_version`; `OrderAcceptanceRecorded` is published by a separate
acceptance transition on either sales path. One commit
cannot be two transition rows and cannot enqueue two producer messages without breaking the
engine's one-message-per-event-declaring-transition invariant
([`../DECISIONS.md`](../DECISIONS.md) D-16). Initial buyer submit needs no second call.

An amendment **MUST NOT** inherit acceptance or treat the amending actor as the accepting buyer,
even on the self-service path. The new immutable version requires a new buyer action through
`POST /acceptance` with its `expected_version` on **both** sales paths. The response/read surface
identifies the current version and whether it has acceptance; prior records remain visible as
history and cannot satisfy the current guard. Administrative updates and state-only transitions
retain the same commercial version and therefore do not invalidate its acceptance. Acceptance
of mutable `draft` content is inadmissible (`not-admissible`). Every separately recorded
`OrderAcceptanceRecorded` carries `accepted_version`, instant, actor and requirement source;
Workflow ignores a historical version's acceptance when considering current fulfillment.

This version-bound rule changes the earlier PRD/design wording of one instant per order and
partner-only separate recording. It is the proposed correction for OL-26; reconciliation of
the Lifecycle and Workflow PRDs remains an upstream requirement, not a claim of Product approval.

On the **partner-placed** path, acceptance **MUST** be recorded as a separate first-class
instant, publishing `OrderAcceptanceRecorded`. The instant **MUST** be the engine's single
pre-write server transition timestamp `t` (`01 §3.6` *Attempt Transition*), committed with the
transition — never a caller-supplied value, and not the physical commit time.

**Only a principal of the `resourceTenantId` party with acceptance-recording permission may
record an order's acceptance instant.** The bar keys on stored facts, never on the current
actor: the path is `orders_order.sales_path` (set at create by `01 §3.7`'s proof-reference rule, D-140), and "who created or submitted it" is
`orders_order_version.actor` of version 1 (the creator), of the submitted version (the
submitter) and of `expected_version` (its amender, where an amendment appended it) (D-106), with
that version's `orders_order_version.actor_tenant_id`. The actor who created, submitted or amended
it is barred where the order is partner-placed **or** that actor's recorded subject tenant differs
from `resourceTenantId` (D-146), so a partner who began delegating after a `self_service` create
is barred too, refused
`acceptance-recording-party-barred` by §3.6 *Record Acceptance* step 4 (D-130); a Seller Operator
has no acceptance-recording authority. The comparison is Lifecycle's, not the PDP's, because the
PDP sees request context and never these stored actors (D-111); `resourceTenantId` membership and
the `acceptance × record` grant remain the PDP pre-guard's. On a
self-service order, the resource-tenant buyer who originally created or submitted it **may**
record renewed acceptance after amendment, subject to the same explicit permission and current
version guards. Initial submit alone confers no separate endpoint permission. This prevents the commercially interested placing or
selling party supplying the customer's consent without a specified, verifiable authority artifact
([`../DECISIONS.md`](../DECISIONS.md) D-31). The permission declaration in
[`08-read-and-authz`](./08-read-and-authz.md) §4.3 carries the rule; this slice carries the guard
that enforces it.

**The bar creates a platform precondition, and it is this slice's to state.** The rule above is
the only technical control against a partner manufacturing customer consent, and it is kept — but
it means the partner path **cannot complete without a `resourceTenantId` principal who can reach
an acceptance surface**. In partner-led selling the end customer frequently has no platform
credential at the point of sale, and where that is so nobody is permitted to record acceptance:
begin-fulfillment refuses with `acceptance-required-not-recorded` (§3.6), the order rests in
`approved`, and it leaves only by its TTL — or, where that TTL is unset, **not at all**
([`07-hold-and-expiry`](./07-hold-and-expiry.md) §4.2, `../DECISIONS.md` Q-27). A partner-placed
order can therefore be commercially agreed offline and still be unfulfillable.

Two things follow. The platform **MUST** be able to present an acceptance action to a
`resourceTenantId` principal for any order the partner path produces — an onboarding or
invitation capability this gear does not own and cannot supply. And where consent is genuinely
captured out of band (a signed document, an email confirmation), recording it **still requires**
a `resourceTenantId` principal to act; this design offers **no** delegated or operator-attested
route, deliberately, because an attested route is exactly the authority artifact D-31 found
unspecified. The acceptance-required election is keyed `(election, scope, scope_id)` only — it has no
sales-path dimension — so absent a contract declaration it is a seller policy (§4.1) that applies
to all of that seller's orders on both paths, and the simplest available mitigation is a seller-scope election of acceptance **not**
required — which the seller requests through platform operations and which takes effect by
deployment promotion, not by a runtime call (§3.7, D-133). It is a commercial decision about
evidence, not a workaround, and should be made knowingly. The reconciliation is routed as [`../DECISIONS.md`](../DECISIONS.md) **Q-30**.

The line-level **acceptance due date is a calendar field** and **MUST NOT** satisfy the instant
under any circumstance. The cascade in [`02-capture`](./02-capture.md) §4.2 that fills it from
the contract-effective date **MUST NOT** be read as defaulting the instant. This is stated twice
across two slices deliberately, because it is the one place where a convenience default would
manufacture consent.

### 4.3 Authorization as a guard input (normative)

The payment-authorization outcome **MUST** be consumed as a begin-fulfillment guard input
supplied by the sibling gear, and **MUST NOT** be stored as an order fact. **Pending** and
**failed** are distinct outcomes and **MUST NOT** be collapsed: pending leaves the order
`approved` with begin-fulfillment uncalled, and failed is submitted to Lifecycle, which leaves
the order `approved` unless the seller has elected tolerate-failure.

**One policy owner.** Lifecycle alone reads `orders_policy_election` and decides whether a
failed authorization is tolerated. Workflow **MUST** submit a conclusive `authorized` or `failed`
outcome to begin-fulfillment once other process prerequisites are satisfied; it **MUST NOT**
infer, cache, or pre-evaluate the seller election. Should a `pending` outcome nonetheless be
submitted, Lifecycle **MUST** refuse it `authorization-pending` without state change; this branch
is defensive, not a protocol step (D-131). An `authorization-failed` refusal parks the
process for operator remediation under its existing manual-task policy; an admitted result is
the only permission to continue. A Workflow call is an attempt, not evidence of admission.
The existing Workflow PRD §6.3 instruction to withhold this call based on seller policy must be
reconciled with this ownership rule; no new policy read API or second policy copy is introduced.

**Pending has a durable continuation.** Workflow **MUST** checkpoint the authorization request
identity, order/version, process correlation, first-pending instant, next-check deadline and
retry budget in its own durable process state. It arms a durable timer using the same selected
execution infrastructure as its approval/date waits, then reads the same authorization request
again when due. This is continuation of an existing process, not an additional Lifecycle
start event and not a new charge/authorization on each poll. A pending answer re-arms the timer
within a finite configured interval/elapsed-time budget (mandatory Workflow configuration,
validated positive and finite before this path is enabled); unavailability consumes the existing
dependency retry budget. Exhaustion creates one inspectable manual task and operator alert,
with order/version/request correlation, while the order remains `approved`. It never admits
fulfillment or fabricates a failed payment outcome. This deadline **MUST NOT** depend on an
optional Lifecycle TTL; if that TTL is configured, escalation precedes its expiry.

Before each wake-up and before forwarding a resolved outcome, Workflow re-reads Lifecycle:
terminal or superseded versions retire this continuation; `on_hold` suspends authorization
processing until resume, retaining the checkpoint and remaining retry budget rather than
starting a fresh budget; only the matching current `approved` version may attempt fulfillment.
Timer delivery is idempotent against the durable checkpoint. Recovery after restart resumes an
overdue checkpoint; buyer acceptance need not arrive to wake pending authorization. Tests must
cover pending→authorized with no acceptance event, restart, repeated pending until exhaustion,
hold/resume, amendment, and terminalization during a wait.

The Payments read-by-request contract and Workflow's durable-execution ADR are still missing
platform prerequisites; this text specifies their required behavior and does not claim an
implemented timer. Reuse the selected Workflow infrastructure and its existing manual-task path;
do not add an Orders Lifecycle scheduler or a custom platform execution mechanism. Pricing's
`Module::serve`/`infra/repricing.rs` cancellation-aware workers are evidence of runtime worker
integration, not a reusable durable payment timer. These upstream gaps remain launch blockers
for a functioning pending-authorization path.

Where tolerate-failure is elected and authorization failed, begin-fulfillment **MAY** proceed and
the risk **MUST** be flagged on the order and audited. The flag records a decision taken at an
instant and **MUST NOT** be cleared later.

**Where the election is stored.** Both begin-fulfillment policy inputs are **policy rows, not
code defaults**, held in `orders_policy_election` (this slice, §3.7) keyed
`(election, scope, scope_id)` with `scope` in (`platform`, `seller`) and seller scope overriding
platform; for acceptance, a referenced contract's declaration overrides both (§4.1 precedence,
D-107). The two elections are `tolerate_authorization_failure` and `acceptance_required`; both change
only by deployment promotion through the policy channel, a seller's election being requested
through platform operations (§3.7, D-133). An
**unset** election is read as its safe value — tolerate-failure **not** elected, acceptance
**required**.
This matters because the design previously specified both as reads with no source: no table, no
key, no scope, no default and no delivery path, so an implementer would have invented a default —
which §2.1 forbids outright for acceptance ([`../DECISIONS.md`](../DECISIONS.md) D-66).

No order state is introduced for any authorization outcome. The rejected alternative was a twelfth,
`payment_pending` state; it was rejected because it would need its own TTL, its own guards, its
own event and its own place in the transition table, all to represent a condition that is
external, transient and already visible in the sibling gear's process state. **Credit scoring is
out of scope and stays out of scope.**

### 4.4 What this design cannot express (normative statement of limitation)

Only **provision-then-collect** is expressible: an authorization read after `approved`, with
at-sale money posted downstream when Subscriptions emits billable facts at activation. A
self-service card checkout **inverts** this — the buyer is charged at checkout and expects
service only if the charge succeeds — and the difference is structural rather than a tuning
parameter, because capture, strong-customer-authentication challenges, retry with an alternative
instrument, and refund-as-reversal have no owning capability.

Two consequences follow and are recorded rather than discovered. A **declined instrument** leaves
the order `approved` with no payment event, no re-authorize operation and expiry as its only
automatic exit, indistinguishable from a healthy order awaiting its next process step.
**Compounded with the unset TTL of `../DECISIONS.md` Q-27 it has no automatic exit at all**, and
only a caller-driven cancel retires it — so the two open questions interact, and Q-08 should not be
read as "the order expires eventually" while Q-06 is unanswered. The Workflow manual-task
escalation required by §4.3 makes this wait actionable to operators once that upstream path
exists, but adds no payment outcome to the order document and no customer re-authorization
surface. Those missing capabilities remain launch-relevant prerequisites; specifying process
recovery here does not implement Payments or resolve its customer experience. And a
**reversal after capture** would be a refund through a payment provider rather than a Billing
credit note, which is a different artifact with a different system of record than the compensation
path assumes.

Neither is a PRD open question: PRD §15's fifteen rows carry nothing about payment ordering, a
declined instrument's exit, or refund-as-reversal. Both are routed in this gear's own register as
[`../DECISIONS.md`](../DECISIONS.md) Q-08, owned by Architecture with Product, and a PRD amendment
adding the row is the ask. Closing either requires a
Payments capability specification that does not exist in this repository, and this design
**MUST NOT** invent one.

## 5. Traceability

- **PRD**: [`../PRD.md`](../PRD.md) — §6.1 buyer acceptance and payment-authorization precondition
- **Gear design**: [`../DESIGN.md`](../DESIGN.md) — realises `cpt-cf-bss-orders-lifecycle-component-preconditions`
- **Engine**: [`01-foundation`](./01-foundation.md) — acceptance table, transition contract, audit
- **Depends on**: [`02-capture`](./02-capture.md) for the acceptance-due-date cascade this slice must not be confused with; [`03-gate-and-pin`](./03-gate-and-pin.md) because the self-service acceptance instant is a contribution to the submit transition that slice owns
- **Consumers**: [`06-workflow-seam`](./06-workflow-seam.md) composes both guards into begin-fulfillment; [`07-hold-and-expiry`](./07-hold-and-expiry.md) owns the TTL that is a declined instrument's only exit
- **ADRs**: [`ADR/0001`](../ADR/0001-cpt-cf-bss-orders-lifecycle-adr-transition-through-engine.md) transition through the engine; [`ADR/0002`](../ADR/0002-cpt-cf-bss-orders-lifecycle-adr-slice-decomposition.md) the foundation-plus-seven-slices decomposition
