# Feature: Buyer Acceptance and the Money Gate

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-featstatus-preconditions-implemented`
- [ ] `p1` - `cpt-cf-bss-orders-lifecycle-feature-preconditions`

## Table of Contents

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows](#2-actor-flows)
  - [2.1 Submit as the accepting buyer](#21-submit-as-the-accepting-buyer)
  - [2.2 Record acceptance of the current version](#22-record-acceptance-of-the-current-version)
  - [2.3 Attempt fulfillment with a payment outcome](#23-attempt-fulfillment-with-a-payment-outcome)
- [3. Processes / Business Logic](#3-processes--business-logic)
  - [3.1 Resolve the acceptance requirement](#31-resolve-the-acceptance-requirement)
  - [3.2 Bind assent to actor and commercial version](#32-bind-assent-to-actor-and-commercial-version)
  - [3.3 Evaluate begin-fulfillment preconditions](#33-evaluate-begin-fulfillment-preconditions)
  - [3.4 Pending authorization integration contract](#34-pending-authorization-integration-contract)
- [4. States](#4-states)
  - [4.1 Acceptance and authorization effects](#41-acceptance-and-authorization-effects)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Acceptance implementation](#51-acceptance-implementation)
  - [5.2 Policy and money-guard implementation](#52-policy-and-money-guard-implementation)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Detailed Behavior Contracts](#7-detailed-behavior-contracts)
  - [Preconditions: Interactions and Sequences](#preconditions-interactions-and-sequences)
  - [Preconditions: The acceptance-required election (normative)](#preconditions-the-acceptance-required-election-normative)
  - [Preconditions: Acceptance on the two paths (normative)](#preconditions-acceptance-on-the-two-paths-normative)
  - [Preconditions: Authorization as a guard input (normative)](#preconditions-authorization-as-a-guard-input-normative)
  - [Preconditions: Traceability](#preconditions-traceability)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Record actual buyer acceptance against an immutable commercial version and contribute acceptance and payment-authorization guards to begin-fulfillment. Acceptance is retained evidence; payment authorization is a transient input supplied by Workflow.

### 1.2 Purpose

**Requirements**: `cpt-cf-bss-orders-lifecycle-fr-order-acceptance`, `cpt-cf-bss-orders-lifecycle-fr-order-payment-auth`, `cpt-cf-bss-orders-lifecycle-fr-order-create`, `cpt-cf-bss-orders-lifecycle-fr-orders-boundary-r3-provisioning`, `cpt-cf-bss-orders-lifecycle-nfr-order-audit-completeness`, `cpt-cf-bss-orders-lifecycle-nfr-order-transition-latency`, `cpt-cf-bss-orders-lifecycle-nfr-order-idempotency`.

**Principles**: `cpt-cf-bss-orders-lifecycle-principle-acceptance-never-defaulted`, `cpt-cf-bss-orders-lifecycle-principle-agreement-not-delegation`, `cpt-cf-bss-orders-lifecycle-principle-authorization-read-not-owned`.

### 1.3 Actors

| Actor | Role |
|-------|------|
| `cpt-cf-bss-orders-lifecycle-actor-orders-direct-customer` | Submit as buyer or record current-version acceptance with the required resource-tenant grant. |
| `cpt-cf-bss-orders-lifecycle-actor-orders-partner-admin` | Place an order; delegation alone cannot supply the customer's consent. |
| `cpt-cf-bss-orders-lifecycle-actor-orders-workflow` | Supply conclusive authorization outcomes at begin-fulfillment and own pending continuation. |
| `cpt-cf-bss-orders-lifecycle-actor-orders-contracts` | Supply the live acceptance-required declaration where a contract is referenced. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md), §6.1 buyer acceptance and payment authorization.
- **Architecture**: [DESIGN.md](../DESIGN.md), including [05 models](../DESIGN.md#contract-05-3-1), [05 interfaces](../DESIGN.md#contract-05-3-3), and [05 persistence](../DESIGN.md#contract-05-3-7). Complete behavior is in [§7](#7-detailed-behavior-contracts).
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md).
- **Dependencies**: [Foundation](01-foundation.md), [Capture](02-capture.md), [Gate and Pin](03-gate-and-pin.md).
- **Consumer**: [Workflow Seam](06-workflow-seam.md).

Schemas and architecture remain in the design. [UPSTREAM_REQS.md](../UPSTREAM_REQS.md) retains the unexposed contract acceptance SDK, Payments read-by-request contract and Workflow durable-execution prerequisites. Version-bound acceptance remains the proposed OL-26 PRD reconciliation. [DECISIONS.md](../DECISIONS.md) Q-08 (Payments limitations), Q-27 (unset TTL) and Q-30 (reachable customer acceptance) remain open; this feature does not supply a payment system or acceptance UI.

**UI applicability**: UI layout, keyboard navigation, screen-reader behavior and visual accessibility are not applicable because this feature specifies backend contracts, not a user interface. API usability, actionable errors and non-disclosing diagnostics remain applicable; consuming consoles own their UI requirements.

## 2. Actor Flows

**Use cases**: `cpt-cf-bss-orders-lifecycle-usecase-order-new-acquisition`, `cpt-cf-bss-orders-lifecycle-usecase-order-fulfillment-complete`.

### 2.1 Submit as the accepting buyer

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-flow-preconditions-submit-acceptance`

**Actor**: Direct Customer submitting an authorized order.

1. [ ] Let Gate and Pin run normal submit. Determine automatic acceptance from the allowed request: no delegation proof reference, and trusted submitter subject tenant equals the order's resource tenant.
2. [ ] If both conditions hold, contribute acceptance for the newly appended submitted version using the trusted actor, engine server transition timestamp, resolved requirement source and `recording_path = self_service`.
3. [ ] Commit acceptance with submit, publishing only `OrderSubmitted`, containing `accepted_version` and acceptance instant. Do not execute a second transition or publish a second event.
4. [ ] Otherwise record no automatic acceptance, even when the stored `sales_path` is `self_service`; require the separate flow where acceptance policy demands it.

**Error scenarios**: A failed submit creates no acceptance. A supplied proof reference suppresses automatic acceptance even on an own-tenant request under the current proxy rule; the unresolved PDP proof-identification and Q-30 constraints remain visible.

### 2.2 Record acceptance of the current version

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-flow-preconditions-record-acceptance`

**Actor**: A resource-tenant principal authorized for `acceptance × record` and not barred by stored placing-party facts.

1. [ ] Call `POST /bss-orders-lifecycle/v1/orders/{orderId}/acceptance` with expected version and idempotency key; the actor and instant are never accepted from caller claims.
2. [ ] Apply Foundation boundary validation, authorization, idempotency, admissibility and version checks. Reject `draft` and terminal orders with `not-admissible`.
3. [ ] Resolve the live requirement and recording-party inputs under §3.1–§3.2. Allow voluntary recording when policy does not require it.
4. [ ] Contribute one append-only acceptance row and request the state-only engine transition. Atomically audit and enqueue `OrderAcceptanceRecorded` with version, instant, actor and source.
5. [ ] Return the stored outcome. Same-key replay reuses it; an independent attempt for an already accepted version refuses `acceptance-already-recorded`.
6. [ ] Serve `GET /orders/{orderId}/acceptance` through Read and Authorization's common wrapper under `order × read`, with access logging, showing the current version's acceptance or absence and historical records.

**Success**: Both initial partner-path and renewed post-amendment acceptance can be recorded by an eligible customer. **Errors**: Missing permissions, barred recording actor, stale version, existing current-version record or unavailable policy input never manufacture acceptance.

### 2.3 Attempt fulfillment with a payment outcome

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-flow-preconditions-begin`

**Actor**: Orders Workflow service principal.

1. [ ] Workflow obtains payment authorization. While pending, it uses its own durable continuation (§3.4) and does not call begin-fulfillment.
2. [ ] Once other process prerequisites are met, Workflow sends conclusive `authorized` or `failed` to the seam's begin-fulfillment operation; it does not read or copy Lifecycle's seller election.
3. [ ] Lifecycle resolves acceptance and tolerance inputs and evaluates §3.3 within the engine's guarded transition contract.
4. [ ] Return admission, admission with a durable risk flag, or the specific refusal. A refusal leaves the order `approved`; Workflow's existing manual-task path handles authorization failure.

## 3. Processes / Business Logic

### 3.1 Resolve the acceptance requirement

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-algo-preconditions-requirement`

**Input**: Current order/version, referenced contract and scoped policy resolution.

**Output**: Current acceptance requirement and source, or an unevaluable guard input.

1. [ ] Read the referenced contract's `acceptance_required` live through Gate and Pin's contract-resolution port when a contract exists; do not reuse a gate-time snapshot.
2. [ ] Otherwise use seller-scope election, then platform-scope election, then the safe fallback `required = true` only when neither row exists.
3. [ ] Treat unavailable contract or policy resolution as `acceptance-requirement-unevaluable`, settled by the engine; an unavailable/unexposed SDK is not an unset election.
4. [ ] When recording required acceptance, persist source `contract`, `seller` or `platform_default`; explicit platform election and fallback share the last value. If not required, persist `volunteered`.
5. [ ] Keep recorded source immutable when policies subsequently change. Resolve current policy again at begin-fulfillment.

### 3.2 Bind assent to actor and commercial version

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-algo-preconditions-record`

**Input**: Expected version, trusted actor, stored role-version facts and existing acceptance.

**Output**: Version-bound acceptance contribution and ordered guard inputs for engine evaluation.

1. [ ] Read the stored creator version (1), submitted version and current amendment version where applicable. Compare the trusted actor to each role version independently.
2. [ ] Resolve the recording-party guard input as failing with `acceptance-recording-party-barred` when that actor matches a role version and either the immutable order sales path is `partner_placed` or that role version's actor tenant differs from the resource tenant. Do not infer eligibility from the current request or `self_service` alone.
3. [ ] Keep resource-tenant membership and the acceptance grant in the PDP pre-guard. Seller Operators have no acceptance-recording authority. An own-tenant buyer on a self-service order may renew acceptance after amendment with the explicit grant.
4. [ ] Check for a row keyed `(order_id, expected_version)`. Old records neither block new recording nor satisfy the current guard. Submit both inputs to the engine: it evaluates not-already-recorded before recording-party, as declared in the [Record Acceptance contract](#contract-05-3-6). Do not return a refusal during input preparation.
5. [ ] Contribute `accepted_version`, trusted actor, engine timestamp `t`, stored sales path for separate recording and resolved source. The final version check prevents a racing amendment from receiving assent intended for its predecessor.
6. [ ] Enforce the unique version-bound key and foreign key. Never default, backfill or infer `accepted_at` from an acceptance due date; no caller-supplied instant is stored.

### 3.3 Evaluate begin-fulfillment preconditions

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-algo-preconditions-evaluate`

**Input**: Current version, resolved policies, its acceptance row or absence, authorization outcome.
**Output**: Guard contribution: admit, admit with tolerance risk, or registered refusal.

1. [ ] Resolve policy inputs before the transaction; use the engine's input-failure path for unavailable inputs.
2. [ ] If acceptance is required and the current version lacks its record, refuse `acceptance-required-not-recorded`.
3. [ ] For `authorized`, admit. For defensive `pending` input, refuse `authorization-pending` without a state change.
4. [ ] For `failed`, resolve `tolerate_authorization_failure` from seller scope then platform scope, with unset meaning false. Refuse `authorization-failed` unless elected.
5. [ ] If elected, contribute the tolerance decision; the engine writes `authorization_failure_tolerated_at` and audits the admission under the `begin-fulfillment` transition reason. `authorization-failed-tolerated` identifies the admission risk, not a refusal or a replacement audit reason. Never clear the flag or store the authorization outcome, instrument or token as an order fact.

### 3.4 Pending authorization integration contract

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-algo-preconditions-pending-continuation`

**Input**: Durable authorization checkpoint, timer delivery and current order/version.

**Output**: Updated checkpoint, matching approved-version continuation, suspension/stop, or manual escalation.

This process is **owned by Workflow and must be implemented there**, not a new Lifecycle worker. It is a required integration behavior whose missing upstream contracts remain blockers.

1. [ ] Workflow checkpoints authorization request identity, order/version, process correlation, first-pending time, next-check deadline and remaining retry budget in durable process state.
2. [ ] Use the selected Workflow durable execution infrastructure to reread the same authorization request on timer wake-up; never create a fresh charge or authorization for each poll.
3. [ ] Validate finite positive pending interval/elapsed-time configuration before enabling this path. Dependency outage consumes its dependency retry budget; exhaustion creates one inspectable manual task and alert without fabricating a failed outcome.
4. [ ] Reread Lifecycle on wake-up and before forwarding: stop for terminal/superseded versions, suspend on hold while retaining remaining budget, and attempt fulfillment only for the matching current `approved` version.
5. [ ] Deduplicate timer delivery by checkpoint and resume overdue checkpoints after restart. Escalation is independent of optional Lifecycle TTL and precedes expiry where configured; it does not wait for an acceptance event.

## 4. States

### 4.1 Acceptance and authorization effects

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-state-preconditions-version-bound`

| Condition | Effect |
|-----------|--------|
| Successful eligible buyer submit | Record acceptance on the new submitted version within submit. |
| Separate recording, any non-terminal state except `draft` | Preserve order state and version; append acceptance and publish its event. |
| Amendment creates N+1 | Keep N's evidence; N+1 starts without acceptance on either sales path. |
| Administrative edit or state-only transition | Existing version-bound acceptance remains valid for that version. |
| Begin-fulfillment with guards satisfied | `approved → in_fulfillment`, owned by Workflow Seam. |
| Pending, non-tolerated failure or missing required acceptance | Remain `approved`; no payment state or payment outcome event. |

A declined instrument has no automatic exit while `approved` TTL is unset. Only provision-then-collect is expressible; payment capture, SCA, reauthorization UI, refunds and chargebacks remain outside this feature.

## 5. Definitions of Done

### 5.1 Acceptance implementation

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-dod-preconditions-acceptance`

The system **MUST** implement both acceptance flows, current-version uniqueness, stored-actor checks and authorized history without inferred consent.

**Implements**: `cpt-cf-bss-orders-lifecycle-flow-preconditions-submit-acceptance`, `cpt-cf-bss-orders-lifecycle-flow-preconditions-record-acceptance`, `cpt-cf-bss-orders-lifecycle-algo-preconditions-requirement`, `cpt-cf-bss-orders-lifecycle-algo-preconditions-record`.
**Constraints**: `cpt-cf-bss-orders-lifecycle-constraint-delegation-proof-required`, `cpt-cf-bss-orders-lifecycle-constraint-read-fails-closed`, `cpt-cf-bss-orders-lifecycle-constraint-bounded-page-size`. Acceptance-history reads inherit the common read wrapper; neither delegation nor a due date constitutes assent.

**Touches**: acceptance POST/GET; submit contribution; `cpt-cf-bss-orders-lifecycle-dbtable-acceptance`; `OrderSubmitted`, `OrderAcceptanceRecorded`.

### 5.2 Policy and money-guard implementation

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-dod-preconditions-money-gate`

The system **MUST** implement policy precedence, safe unset values and fail-closed outages, with the risk flag written only by admitted begin-fulfillment. Deliver policy rows through deployment promotion requested via platform operations; no runtime write endpoint or new PDP action exists. Enforce platform-key uniqueness with `NULLS NOT DISTINCT` and the scope/nullability rules in the design. Instrument authorization outcomes separately, tolerated admissions, acceptance latency and recording-party/unevaluable refusals with the design's alerts.

**Implements**: `cpt-cf-bss-orders-lifecycle-flow-preconditions-begin`, `cpt-cf-bss-orders-lifecycle-algo-preconditions-evaluate`, `cpt-cf-bss-orders-lifecycle-algo-preconditions-pending-continuation` (integration evidence).
**Constraints**: `cpt-cf-bss-orders-lifecycle-constraint-no-payment-pending-state`, `cpt-cf-bss-orders-lifecycle-constraint-declined-instrument-exit`, `cpt-cf-bss-orders-lifecycle-constraint-no-payment-collection`.
**Touches**: `cpt-cf-bss-orders-lifecycle-dbtable-policy-election`; `orders_order.authorization_failure_tolerated_at`; begin-fulfillment guard composition; Workflow contract fixtures.

## 6. Acceptance Criteria

- [ ] With both current-version acceptance and a barred recording actor, an independently authorized different-key attempt returns `acceptance-already-recorded`; same-key replay retains its stored result.

- [ ] Own-tenant submit without proof records one acceptance and one `OrderSubmitted`; delegated submit records none, including delegation begun after self-service creation.
- [ ] An acceptance due date, migration default or caller timestamp cannot produce assent. Separate recording uses the engine timestamp and trusted actor.
- [ ] Contract, seller, platform and unset policies resolve in order; contract outage refuses without falling back. Optional acceptance is recordable as `volunteered`.
- [ ] Barred placing actors and seller operators cannot record customer acceptance. Per-role checks still allow an otherwise eligible own-tenant creator after a different delegated submitter acted.
- [ ] Concurrent same-key recordings replay one result; independent duplicate attempts cannot create a second version-bound row. An amendment race cannot bind consent to the wrong version.
- [ ] After amendment, old acceptance is readable but cannot admit the new version. Administrative edits and state-only transitions preserve current-version acceptance.
- [ ] The acceptance/authorization matrix covers required-missing, authorized, defensive pending, failed-unset, failed-false and failed-tolerated outcomes; only the last writes the permanent risk flag.
- [ ] Policy rows cannot acquire duplicate platform entries; promotion metadata records change actor/time and no public operation mutates policy.
- [ ] Workflow contract tests cover pending→authorized without acceptance events, restart, duplicate timers, repeated-pending exhaustion, hold/resume, amendment and terminalization. These are future integration evidence, not claims of implemented upstream timers.
- [ ] Acceptance commits are atomic with audit and event enqueue, meet p95 < 1 s, and replay creates no additional effect. No payment outcome, token or instrument becomes authoritative order data.

## 7. Detailed Behavior Contracts

**Contract namespace 05.** Section numbers inside the detailed contracts below resolve
through the [contract address index](../DECOMPOSITION.md#4-contract-address-index),
not the overview section numbers above.

The following sections retain the complete algorithms, guards, state transitions and
feature-local rules. The preceding flows and acceptance criteria summarize these contracts;
shared schemas and interfaces are defined in [DESIGN.md](../DESIGN.md).

<a id="contract-05-3-6"></a>

<!-- contract:05-preconditions:3.6 -->
### Preconditions: Interactions and Sequences

<a id="contract-05-record-the-acceptance-instant"></a>

#### Record the acceptance instant

**ID**: `cpt-cf-bss-orders-lifecycle-seq-record-acceptance`

**Use cases**: `cpt-cf-bss-orders-lifecycle-usecase-order-new-acquisition`

**Actors**: `cpt-cf-bss-orders-lifecycle-actor-orders-partner-admin`, `cpt-cf-bss-orders-lifecycle-actor-orders-direct-customer`

**Algorithm: Record Acceptance**

Input: order_id, expected_version, recording_actor, security_context, idempotency_key
Output: recorded, or a registered refusal

1. [ ] - `p1` - Declare guards the engine evaluates and audits: submitted non-terminal state (never `draft`), expected_version current (engine `version-conflict`), not-already-recorded for that version (step 5), and recording-party (step 4, `acceptance-recording-party-barred`) - `inst-ra-declare-guards`
2. [ ] - `p1` - Resolve whether acceptance is required by the §4.1 precedence: the referenced contract's declaration where a contract is referenced (the `acceptance_required` field of the [03 §3.3](../DESIGN.md#contract-03-3-3) contract-resolution port, called live outside the gate, D-132), else the seller-scope election, else the platform-scope election, else the safe fallback (required). If contract/policy resolution is unavailable, contribute acceptance-requirement-unevaluable to the engine's ordinary input-failure branch; do not interpret outage as an unset safe fallback - `inst-ra-resolve-requirement`
3. [ ] - `p1` - Permit recording on both sales paths, including renewed acceptance after amendment; resolve `requirement_source` to `contract`, `seller`, `platform_default` or `volunteered` (§4.1) — `seller` where a seller-scope election decided it, `platform_default` where the platform-scope election or the unset fallback did - `inst-ra-resolve-path-admissibility`
4. [ ] - `p1` - Resolve recording-party from stored facts only (§4.2, D-130, D-146): take the role versions — version 1 (the creator), the submitted version (the submitter) and `expected_version` (its amender, where that version was appended by an amendment). The guard refuses with `acceptance-recording-party-barred` when the trusted SecurityContext actor equals the `orders_order_version.actor` of a role version and either `orders_order.sales_path = partner_placed` ([01 §3.7](../DESIGN.md#contract-01-3-7), D-140) or that role version's `orders_order_version.actor_tenant_id` differs from `orders_order.resource_tenant_id`. The bar is applied per role version, so a buyer who created its own order is not barred merely because a delegated partner submitted it. Otherwise the guard passes; `sales_path = self_service` alone never passes it (D-146). `resourceTenantId` membership and the `acceptance × record` grant are not re-checked here: they stay with the engine's PDP pre-guard (`operation-not-permitted-for-actor`, or `order-not-found` per D-114), which cannot see stored version actors (D-111) - `inst-ra-resolve-recording-party`
5. [ ] - `p1` - Resolve not-already-recorded by reading `(order_id, expected_version)`; an existing row for this version refuses with `acceptance-already-recorded`, while older rows do not block recording. Same-key replays use the engine's stored response - `inst-ra-resolve-already-recorded`
6. [ ] - `p1` - Contribute the engine's server transition timestamp, `accepted_version = expected_version`, recording actor from trusted SecurityContext, recording_path copied from the immutable `orders_order.sales_path` (D-106, D-140) and requirement source to the engine transaction; the final version check prevents a racing amendment from receiving assent intended for its predecessor - `inst-ra-record-instant`
7. [ ] - `p1` - Request the state-only acceptance transition so the engine audits it and publishes OrderAcceptanceRecorded - `inst-ra-request-transition`
8. [ ] - `p1` - **RETURN** recorded - `inst-ra-return-recorded`

**Description**: The instant is the engine's pre-write server timestamp committed with the transition, not a prediction of physical commit time or a caller-supplied value, because a
caller-supplied instant is a claim rather than an observation. The event is what lets the
sibling gear re-evaluate begin-fulfillment eligibility without polling.

<a id="contract-05-self-service-submit-constitutes-acceptance"></a>

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
([01 §3.6](01-foundation.md#contract-01-3-6) *Attempt Transition*) and resolved requirement_source.
Separate recording copies `orders_order.sales_path` (written once at create, D-106 — `partner_placed` iff the allowed create carried a delegation proof reference, [01 §3.7](../DESIGN.md#contract-01-3-7), D-140), never infers it from the current actor.

<a id="contract-05-begin-fulfillment-guard-evaluation"></a>

#### Begin-fulfillment guard evaluation

**ID**: `cpt-cf-bss-orders-lifecycle-seq-begin-fulfillment-guards`

**Use cases**: `cpt-cf-bss-orders-lifecycle-usecase-order-fulfillment-complete`

**Actors**: `cpt-cf-bss-orders-lifecycle-actor-orders-workflow`, `cpt-cf-bss-orders-lifecycle-actor-orders-contracts`

**Algorithm: Evaluate Begin-Fulfillment Preconditions**

Input: order_id, expected_version, authorization_outcome, security_context
Output: admit, admit-with-risk-flag, or a registered refusal

1. [ ] - `p1` - Resolve acceptance and tolerance policies before the transaction, reading a referenced contract's `acceptance_required` live through the [03 §3.3](../DESIGN.md#contract-03-3-3) contract-resolution port (D-132); unavailable contract/policy inputs contribute acceptance-requirement-unevaluable through the engine; only an actual unset election uses its safe fallback - `inst-bg-resolve-requirement`
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


<!-- /contract -->

<a id="contract-05-4-1"></a>

<!-- contract:05-preconditions:4.1 -->
### Preconditions: The acceptance-required election (normative)

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


<!-- /contract -->

<a id="contract-05-4-2"></a>

<!-- contract:05-preconditions:4.2 -->
### Preconditions: Acceptance on the two paths (normative)

**Which submit is a self-service submit (normative, D-146).** The automatic acceptance at submit
is keyed on facts of the submit request itself, never on `orders_order.sales_path`: it is written
**only** when (1) the submit request the engine's authorization allowed carried **no** delegation
proof reference, and (2) the submitting principal's trusted `SecurityContext.subject_tenant_id`
equals the order's `resourceTenantId`, frozen at submit ([02 §4.3](../DESIGN.md#contract-02-4-3)). Otherwise the submit writes no
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
pre-write server transition timestamp `t` ([01 §3.6](01-foundation.md#contract-01-3-6) *Attempt Transition*), committed with the
transition — never a caller-supplied value, and not the physical commit time.

**Only a principal of the `resourceTenantId` party with acceptance-recording permission may
record an order's acceptance instant.** The bar keys on stored facts, never on the current
actor: the path is `orders_order.sales_path` (set at create by [01 §3.7](../DESIGN.md#contract-01-3-7)'s proof-reference rule, D-140), and "who created or submitted it" is
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
[08-read-and-authz — The permission model (normative)](../DESIGN.md#contract-08-4-3) carries the rule; this slice carries the guard
that enforces it.

**The bar creates a platform precondition, and it is this slice's to state.** The rule above is
the only technical control against a partner manufacturing customer consent, and it is kept — but
it means the partner path **cannot complete without a `resourceTenantId` principal who can reach
an acceptance surface**. In partner-led selling the end customer frequently has no platform
credential at the point of sale, and where that is so nobody is permitted to record acceptance:
begin-fulfillment refuses with `acceptance-required-not-recorded` (§3.6), the order rests in
`approved`, and it leaves only by its TTL — or, where that TTL is unset, **not at all**
([07-hold-and-expiry — Bounded lifetime (normative)](07-hold-and-expiry.md#contract-07-4-2), `../DECISIONS.md` Q-27). A partner-placed
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
under any circumstance. The cascade in [02-capture — The date cascade (normative)](02-capture.md#contract-02-4-2) that fills it from
the contract-effective date **MUST NOT** be read as defaulting the instant. This is stated twice
across two slices deliberately, because it is the one place where a convenience default would
manufacture consent.


<!-- /contract -->

<a id="contract-05-4-3"></a>

<!-- contract:05-preconditions:4.3 -->
### Preconditions: Authorization as a guard input (normative)

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


<!-- /contract -->

<a id="contract-05-5"></a>

<!-- contract:05-preconditions:5 -->
### Preconditions: Traceability

- **PRD**: [`../PRD.md`](../PRD.md) — §6.1 buyer acceptance and payment-authorization precondition
- **Gear design**: [`../DESIGN.md`](../DESIGN.md) — realises `cpt-cf-bss-orders-lifecycle-component-preconditions`
- **Engine**: [`01-foundation`](../DESIGN.md#contract-01-1-1) — acceptance table, transition contract, audit
- **Depends on**: [`02-capture`](../DESIGN.md#contract-02-1-1) for the acceptance-due-date cascade this slice must not be confused with; [`03-gate-and-pin`](../DESIGN.md#contract-03-1-1) because the self-service acceptance instant is a contribution to the submit transition that slice owns
- **Consumers**: [`06-workflow-seam`](../DESIGN.md#contract-06-1-1) composes both guards into begin-fulfillment; [`07-hold-and-expiry`](../DESIGN.md#contract-07-1-1) owns the TTL that is a declined instrument's only exit
- **ADRs**: [`ADR/0001`](../ADR/0001-cpt-cf-bss-orders-lifecycle-adr-transition-through-engine.md) transition through the engine; [`ADR/0002`](../ADR/0002-cpt-cf-bss-orders-lifecycle-adr-slice-decomposition.md) the foundation-plus-seven-slices decomposition

<!-- /contract -->
