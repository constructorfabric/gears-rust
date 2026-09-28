<!-- CONFLUENCE_TITLE: [BSS]: Orders Workflow — Upstream Requirements -->
<!-- Related: ./PRD.md, ./DESIGN.md | Owners: BSS Orders team -->

# UPSTREAM_REQS — Orders Workflow

<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Requesting Gears](#12-requesting-gears)
- [2. Requirements](#2-requirements)
  - [2.1 Subscriptions](#21-subscriptions)
  - [2.2 Payments](#22-payments)
  - [2.3 Generic Approval](#23-generic-approval)
  - [2.4 Orders Lifecycle](#24-orders-lifecycle)
  - [2.5 Catalog](#25-catalog)
  - [2.6 Privacy and data classification](#26-privacy-and-data-classification)
  - [2.7 Event Broker](#27-event-broker)
  - [2.8 Platform authorization policy](#28-platform-authorization-policy)
  - [2.9 Serverless runtime (platform durable execution)](#29-serverless-runtime-platform-durable-execution)
- [3. Priorities](#3-priorities)
- [4. Required PRD Amendments](#4-required-prd-amendments)
- [5. Traceability](#5-traceability)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

This register captures the asks Orders Workflow raises on gears and specifications it does not
own: Subscriptions, Payments, and the Generic Approval service. It exists because these asks
previously lived only in PRD prose and forked from the canonical Subscriptions seam map
(`gears/bss/subscriptions/docs/SEAMS.md`). That map defines `SUB-O1` through `SUB-O6`, where
`SUB-O6` means **atomic multi-subscription submission is cheaper than the deferral assumed** (MED,
joint Orders/Subscriptions, reopening `SUB-D-04`/`SUB-C2`). The Orders Workflow PRD independently
reused `SUB-O6` for a **machine-readable in-flight rejection** — a completely different ask — and
invented `SUB-O7`, `SUB-O8`, `SUB-O9`, none of which were ever registered upstream. `SUB-O10` is
already claimed by the sibling Orders Lifecycle design for an explicit subscription-start instant.
The sibling Lifecycle register avoided the collision by declaring `SUB-O4`, `SUB-O6`, `SUB-O7` and
`SUB-O8` "not asks of that gear" — it could do so because all four are, in fact, asks of **this**
gear. Resolving the fork is therefore this gear's responsibility, done here by treating `SEAMS.md`
as canonical and renumbering this gear's four PRD-local asks to `SUB-O11`..`SUB-O14`, each labelled
**UNASKED** (never registered upstream, a stronger and more honest status than "unagreed").

This register also carries two inherited Subscriptions asks already present in `SEAMS.md`
(`SUB-O1`, `SUB-O5`, both unagreed), the sibling Lifecycle's `SUB-O10` precedent (which this gear
also depends on), a Payments ask with no owning specification anywhere in this repository, and a
Generic Approval ask against the PRD's own §9.2 expectations contract, which stands in as the
normative interface until a canonical specification exists. Two further Subscriptions asks,
`SUB-O15` and `SUB-O16`, are raised here for the first time — they are new asks at the next free
numbers, not renumberings of anything. It further carries asks on **Orders
Lifecycle** (visibility of the `submitted` TTL, which a normative MUST in this design depends on
and which this gear cannot see, thin variants of the events the platform consumes for it, and two
missing values of the `failure_reason` enumeration), on **Catalog** (the dependency-topology read every fulfillment
plan is constructed from, a dependency this register did not previously name at all), and on the
**PRD owner** for a privacy and data-classification ruling this design cannot make for itself,
and on the **platform authorization policy owner** for the PDP catalogue registration, role
provisioning and the `assigned_principal` approver grant that every authorized operation in this
design depends on (§2.8, `ADR/0010`, `DECISIONS.md` D-63). Finally, it carries the asks on the
platform gear **serverless-runtime**, which executes the order process definition since `ADR/0011`
(§2.9, `DECISIONS.md` D-65…D-69): delivery and readiness, event triggers over the broker, member-only
storage of trigger inputs and consumed events, the service identity of outbound calls, attempt and deadline propagation, engine-history residency and
retention, definition versioning with a pre-publish validation hook, named signals and an
invocation-preserving re-drive, restriction of start and generic control on the order process,
operator visibility of trigger-path dead letters and a failure-handler safety net.

### 1.2 Requesting Gears

| Upstream target | Agreement status | Why this gear needs it |
|------------------|-------------------|-------------------------|
| Subscriptions (`gears/bss/subscriptions/docs/SEAMS.md`) | `SUB-O1`, `SUB-O5` registered-and-unagreed; `SUB-O10` registered by the sibling Lifecycle design, unagreed; `SUB-O11`..`SUB-O16` UNASKED (never registered) | Provisioning intents, compensation, in-flight status, correlation propagation, the seam's latency budget, and callback attribution all cross this seam; several gaps make parts of the design fail closed or unenforceable until they land. |
| Payments | No specification or register exists in this repository | Begin-fulfillment gating needs an authorization outcome distinguishing authorized/pending/failed; there is no owner to receive the ask. |
| Generic Approval service | No canonical specification; PRD §9.2 expectations contract is the normative interface until one exists | Approval-requirement verdict acquisition, routing, multi-party gates, and escalation are executed against this contract via a phase-1 stand-in that returns `approval not required` (audited), pending the real service. |
| Orders Lifecycle (`gears/bss/orders-lifecycle`) | UNASKED (never registered) | This design's escalation obligation is stated relative to the order's `submitted` TTL, which Orders Lifecycle owns and this gear can neither read nor derive; without it the obligation is a strict inequality between two quantities, only one of which is knowable here. The events the platform consumes for this gear carry Lifecycle's commercial fields into engine history unless a thin variant exists or the platform stores selected members only (ADR-0013). Two failure causes have no value in Lifecycle's closed `failure_reason` enumeration (§2.4). Lifecycle's 24-hour idempotency window is shorter than this gear's re-issue of a transition key (§2.4, D-188). |
| Catalog | UNASKED (never registered; not a PRD-registered actor either) | Every fulfillment plan is constructed from Catalog's dependency topology and frozen against it; the plan cannot be built, validated for cycles, or ordered for compensation without a read contract. |
| Event Broker (`gears/system/event-broker`) | REGISTERED by Orders Lifecycle (`gears/bss/orders-lifecycle/docs/UPSTREAM_REQS.md` §2.7), open; co-signed here | Process events publish through the platform producer outbox (`ADR/0008`, D-58); the runtime, cursor/retry semantics, dead-letter recovery, root tenancy and delivery observability are platform prerequisites this gear cannot report ready without. |
| Platform authorization policy owner (`authz-resolver` PDP provider and policy provisioning) | UNASKED (never registered); Orders Lifecycle's `…-upreq-pdp-policy-integration` (`gears/bss/orders-lifecycle/docs/UPSTREAM_REQS.md` §2.9) is the precedent and the two should be provisioned together | Every operation is authorized by the platform PDP on a registered `(resource, action)` pair through the shared `PolicyEnforcer` adapter (`ADR/0010`, D-63); until the catalogue is registered and the roles, the `assigned_principal` approver grant and the service-principal grants are provisioned and verified against the deployed provider, no caller-driven operation is authorizable in production, and this design fabricates no default grant. |
| serverless-runtime (`gears/serverless-runtime`) | UNASKED (never registered); the platform's own `NEXT_ADR_SCOPE.md` names several of the gaps as open | Since `ADR/0011` the order process flow is a platform workflow definition executed by the serverless-runtime Temporal plugin; the gear has no code today, and the service identity of outbound calls, event triggers over the broker, member-only storage of consumed events, named signals, attempt identity, a pre-publish validation hook, version retention while bound, history residency and dead-letter visibility are not stated by any platform document. Until they land the platform path is not ready and only the fallback property holds (§2.9). |
| PRD owner (privacy / data classification) | UNASKED | The PRD's "Privacy / PII: not applicable" exclusion does not survive contact with a 400-day audit trail carrying actor, operator and approver identities; the ruling is a PRD amendment, not a design change. |

## 2. Requirements

### 2.1 Subscriptions

The canonical seam-map numbering (`SUB-O1`..`SUB-O6`, per `gears/bss/subscriptions/docs/SEAMS.md`
lines 145-165) is treated as authoritative. This gear's PRD §13 previously defined `SUB-O6`
through `SUB-O9` locally; those four numbers are **renumbered and replaced** below as `SUB-O11`
through `SUB-O14`, not aliased. See the translation table in §2.1 ("Translation table") for the old-to-new mapping.

#### Overlap-presence read (inherited, unagreed)

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-upreq-overlap-presence-read`

Subscriptions **MUST** expose an overlap-key presence read: given an overlap scope key and a
payer, does a non-terminal subscription already hold that key, and what is the effective
concurrent-active cardinality. Orders Workflow consumes this at fulfillment-plan construction and
again immediately before the first activation intent (pre-activation abort check, PRD §6.1). This
gear cannot answer it alone because Subscriptions owns subscription state; the check is only
partially evaluable without it.

- **Rationale**: Registered upstream in `SEAMS.md` as `SUB-O5` (HIGH, "neighbour-extends"),
  unagreed. Until it lands, the against-existing-subscriptions half of the pre-activation abort
  check is unevaluable and therefore fails closed: after Lifecycle's `defer` ladder the
  pre-activation re-check halts before activation, voids the wave-1 drafts and records
  `overlap-read-unevaluable`, never `overlap-collision` (`DECISIONS.md` D-91,
  `design/04-fulfillment-plan.md` §4.2).
- **Consequence if it does not land**: the pre-activation overlap check can only ever evaluate the
  within-basket half; the against-existing-subscriptions half remains permanently unevaluable and
  the design must keep treating that half as a fail-closed refusal.
- **Source**: `gears/bss/subscriptions/docs/SEAMS.md` §I (`SUB-O5`); consumed by PRD §6.1
  (Fulfillment Plan Construction, pre-activation abort) and `DESIGN.md` fulfillment-plan slice.

#### Compensation cancellation reason (inherited, critical, unagreed)

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-upreq-compensation-cancel-reason`

Subscriptions **MUST** add a cancellation reason value for order-fulfillment compensation
(`SubscriptionCancelled.reason` is today a closed enum), scoped **out** of the early-termination
class so it derives neither an ETF nor a credit. This gear cannot satisfy it alone because the
enum and its ETF/credit scoping are owned by Subscriptions.

- **Rationale**: Registered upstream in `SEAMS.md` as `SUB-O1` (CRIT, "neighbour-extends"),
  unagreed. The activated-cancel compensation leg (order fulfilment failing after one or more
  lines have already activated) needs a reason that is neither an early termination (no party
  defaulted, no fee owed) nor a plan-change supersession; no existing enum value fits.
- **Consequence if it does not land**: this gear's activated-cancel compensation is forced to
  reuse a reason value that wrongly derives an ETF/credit, or repurpose `saga_superseded`, which is
  reserved for the cancel+new pair of a plan change — either choice misrepresents the compensation
  on the audit trail and on billing-facing events.
- **Note**: reason values ride event payloads and downstream consumers key on them, so adding a
  value after Billing consumes the contract is a **breaking** change — this is the ask materially
  cheaper now than later.
- **Source**: `gears/bss/subscriptions/docs/SEAMS.md` §I (`SUB-O1`); consumed by PRD §6.4
  (cancellation fencing / compensation) and the activated-cancel leg of the fulfillment design.

#### Explicit subscription start instant (raised by sibling Lifecycle design, unagreed)

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-upreq-explicit-start-instant`

Subscriptions' `create` and activation intent **MUST** accept an explicit start instant and
**MUST NOT** derive the subscription start from any date carried on the order. This gear depends
on it as much as Lifecycle does: the activation intent this gear issues must carry the actual
activation instant, because a line deferred past its quoted service-activation date **MUST NOT**
be backdated (PRD §6.1, §6.2 wave-2 activation).

- **Rationale**: Registered upstream as `SUB-O10` by the sibling Orders Lifecycle design, unagreed
  — a new ask, not present in either the seam map or this gear's PRD numbering. Raised here
  because this gear is the one that actually issues the activation intent and therefore the one
  that must supply the instant.
- **Consequence if it does not land**: without an explicit start instant on the intent, a line
  deferred past its quoted activation date is either backdated (violating the PRD's no-backdating
  requirement) or the actual activation instant is lost, making billing and entitlement start
  dates unenforceable from the order side.
- **Source**: `gears/bss/orders-lifecycle/docs/UPSTREAM_REQS.md` §2.1
  (`upreq-subscription-start-instant`, `SUB-O10`); consumed by PRD §6.1/§6.2 wave-2 activation.

#### `SUB-O11` — machine-readable in-flight rejection (UNASKED)

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-upreq-in-flight-rejection`

Subscriptions **MUST** reject a resubmit of an already-accepted, in-flight transition request with
a machine-readable outcome that distinguishes "already accepted" from "not accepted", so a caller
retry can tell which case it is in without guessing from silence or a generic error.

- **Rationale**: Orders Workflow's retry budget applies only to intent-**submission** failures; an
  intent already accepted and in flight must never be retried as a resubmit. Today Subscriptions
  has no contract for this distinction, so a resubmit's rejection is not machine-readable.
- **Consequence if it does not land**: the caller-side duplicate protocol (PRD §6.3) cannot reliably
  distinguish an in-flight duplicate from a genuine submission failure, forcing the sweep-based
  status read (`SUB-O13`) to carry the entire disambiguation burden with no fast-path signal.
- **Agreement status**: **UNASKED** — this identifier has never existed in `SEAMS.md` at any
  number; it is not registered-and-unagreed, it has simply never been raised upstream.
- **Source**: replaces the PRD's local `SUB-O6` (§13 Dependencies, §6.3 retry/duplicate protocol).
  Raised against `gears/bss/subscriptions/docs/SEAMS.md`, which has no such entry today.

#### `SUB-O12` — cancel/void of an accepted transition request (UNASKED)

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-upreq-cancel-accepted-transition`

Subscriptions **MUST** expose a cancel-or-void operation for an accepted, in-flight transition
request, usable as the superseding action for that intent instead of a second submit.

- **Rationale**: Once `SUB-O11` lets Orders Workflow recognize an in-flight duplicate, the only
  correct next action is to supersede that accepted intent, not resubmit it. No such operation is
  contracted today.
- **Consequence if it does not land**: an accepted in-flight intent that needs to be abandoned
  (e.g., on order cancellation or amendment) has no supported path other than waiting for it to
  reach a terminal outcome on its own, which can stall compensation and cancellation fencing.
- **Agreement status**: **UNASKED** — never registered upstream at any number.
- **Source**: replaces the PRD's local `SUB-O7` (§6.3 retry/duplicate protocol, §6.4 cancellation
  fencing). Raised against `gears/bss/subscriptions/docs/SEAMS.md`, which has no such entry today.

#### `SUB-O13` — status-read of a non-terminal intent (UNASKED)

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-upreq-nonterminal-status-read`

Subscriptions **MUST** expose a status-read for a non-terminal provisioning intent, addressable
either by the transition-request identifier or by the full lookup tuple `orderId` +
`orderVersion` + order-line reference + wave + `intentKind` + `wave_attempt` (equivalently, by the
intent idempotency key). The four-component tuple this entry previously stated cannot distinguish
a lapsed draft, its rebuilt successor and a void (`design/05-provisioning-intents.md` §4.1, D-97);
the read also supplies the `subscriptionId`, which therefore no longer needs to ride a
confirmation.

- **Rationale**: the background reconciliation sweep (PRD §Intent Reconciliation Sweep) must, after
  an idempotency key ages out, confirm outcomes by lookup rather than resubmission; this is the
  read-only discovery mechanism cancellation fencing's "reconcile in-flight intents" step depends
  on.
- **Consequence if it does not land**: the sweep cannot drive aged-out intents to a terminal
  confirmation or failure by lookup, so unresolved intents can only be resolved by resubmission
  (which risks duplication) or by parking indefinitely.
- **Agreement status**: **UNASKED** — never registered upstream at any number.
- **Source**: replaces the PRD's local `SUB-O8` (§Intent Reconciliation Sweep, §6.3 caller-side
  duplicate protocol). Raised against `gears/bss/subscriptions/docs/SEAMS.md`, which has no such
  entry today.

#### `SUB-O14` — `correlationId` propagation toward Policy Engine / OSS (UNASKED)

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-workflow-upreq-correlation-propagation`

Subscriptions **MUST** propagate the process `correlationId` it receives on each intent along the
Subscriptions → Policy Engine → OSS path, so it can be echoed on confirmations and observed
end-to-end.

- **Rationale**: Orders Workflow **MUST NOT** invoke OSS Provisioning directly (R3) — the
  Subscriptions → Policy Engine → OSS path is the only permissible provisioning route — so an
  end-to-end acquisition trace depends entirely on Subscriptions relaying the identifier onward.
- **Consequence if it does not land**: an end-to-end trace of a single fulfillment attempt stops at
  the Subscriptions seam; diagnosing a failure inside the Policy Engine or OSS legs cannot be
  correlated back to the originating process instance without a manual join.
- **Agreement status**: **UNASKED** — never registered upstream at any number.
- **Source**: replaces the PRD's local `SUB-O9` (§13 Dependencies, §6.3). Raised against
  `gears/bss/subscriptions/docs/SEAMS.md`, which has no such entry today.

#### Translation table (PRD-local numbers → renumbered asks)

| Old PRD number (local, colliding/unregistered) | New number (this register) | Meaning | Status |
|---|---|---|---|
| `SUB-O6` (PRD-local: in-flight rejection) | `SUB-O11` | Machine-readable in-flight rejection | UNASKED |
| `SUB-O7` | `SUB-O12` | Cancel/void of an accepted transition request | UNASKED |
| `SUB-O8` | `SUB-O13` | Status-read of a non-terminal intent | UNASKED |
| `SUB-O9` | `SUB-O14` | `correlationId` propagation toward Policy Engine / OSS | UNASKED |

Note: `SUB-O6` in `SEAMS.md` (canonical) means **atomic multi-subscription submission is cheaper
than the deferral assumed** (MED, joint Orders/Subscriptions, reopening `SUB-D-04`/`SUB-C2`) — an
entirely different ask from the PRD's former local `SUB-O6` (in-flight rejection, now `SUB-O11`).
This register does not raise the canonical `SUB-O6` reopen as an ask of this gear; it is recorded
here only to make the double meaning explicit and to confirm the collision is fully resolved by
the renumbering above.

#### What the design cannot do until `SUB-O11`–`SUB-O14` land

All four are **UNASKED** — never raised upstream at any number, not merely unagreed. Stated once,
plainly, so no slice has to restate it and no reader of a single slice concludes the mechanism
exists:

| Ask | What is not possible until it lands |
|---|---|
| `SUB-O11` in-flight rejection | A retry cannot tell "already accepted, in flight" from "not accepted". The caller-side duplicate protocol has no fast-path signal and must infer from silence or a generic error, which is exactly the inference that produces double-provisioning. The whole disambiguation burden falls on `SUB-O13`, which is itself UNASKED. |
| `SUB-O12` cancel/void of an accepted transition | An accepted in-flight intent that must be abandoned — on cancellation, amendment, or supersession — has no supported abandonment path. It can only be waited out to its own terminal outcome, which stalls cancellation fencing and compensation for as long as the downstream takes. |
| `SUB-O13` non-terminal status read | The reconciliation sweep cannot confirm an outcome by lookup. After an idempotency key ages out, an unresolved intent can only be resubmitted (risking duplication) or parked indefinitely, so the sweep's read-only switch has nothing to read. |
| `SUB-O14` `correlationId` propagation | An end-to-end trace of a fulfillment attempt stops at the Subscriptions seam; a failure inside the Policy Engine or OSS legs cannot be correlated back to the originating process instance without a manual join. |

Two of these sit inside **mandatory** sequences rather than in optional hardening, which is the
part a reader is most likely to miss: `design/05-provisioning-intents.md` names `SUB-O13` as the
reconciliation sweep's confirmation mechanism, and `design/06-saga-and-compensation.md` names
`SUB-O12` as step 3 of the cancellation-fencing sequence — a sequence that same slice requires to
"run to completion" before any compensated outcome may be reported. Both sequences are therefore
specified against mechanisms that do not exist upstream and have never been asked for. Neither
slice may be read as evidence that they do.

#### `SUB-O15` — latency and throughput budget for the provisioning seam (UNASKED)

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-upreq-provisioning-latency-budget`

Subscriptions **MUST** commit to a latency and throughput objective for the provisioning-intent
seam: a p95 and p99 for accepting an intent and for confirming it, a sustained intent-submission
rate this gear may offer, and the rate-limit or back-pressure signal returned when that rate is
exceeded.

- **Rationale**: the PRD's 15-minute p95 for a standard order is measured over a window in which
  every leg is a Subscriptions round trip, and this design's own fulfillment slice concedes that
  dependency-chain depth dominates the measured interval. The step deadline is currently *derived
  downward* from the SLA rather than *negotiated against* a downstream objective, which means the
  gear has allocated itself a budget out of a total it does not control.
- **Consequence if it does not land**: a slow-but-healthy Subscriptions is converted by this gear
  into failed lines, manual tasks and compensations, because the only bound available is the
  self-imposed step deadline; there is no basis to distinguish "the downstream is within its
  objective and we budgeted wrongly" from "the downstream is failing its objective".
- **Agreement status**: **UNASKED** — no latency, throughput or rate-limit ask exists in
  `SEAMS.md` or anywhere in this register today.
- **Source**: PRD §7 (fulfillment SLA), §6.3 (provisioning intents). Raised against
  `gears/bss/subscriptions/docs/SEAMS.md`, which has no such entry.

#### `SUB-O16` — echo the identity envelope on every confirmation (UNASKED)

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-upreq-identity-envelope-echo`

Subscriptions **MUST** echo, on every confirmation and failure callback, the identity envelope it
received on the originating intent — the union of the process `correlationId`, the intent
idempotency key and the asserting principal with `orderId`, `orderVersion`, `orderLineId`, wave
and the opaque binding reference (`design/05-provisioning-intents.md` §4.1) — so a callback can be attributed to the intent that caused it without a
lookup, and so a callback that echoes nothing this gear issued is rejectable as unattributable.

- **Rationale**: confirmations and failure outcomes arrive over an event/callback transport that
  traverses no REST gateway and carries no service principal of its own, so the design has no
  stated mechanism by which an inbound confirmation is authenticated or attributed. Echoing the
  envelope is the cheapest attribution primitive, and it is the precondition for treating an
  unattributable callback as a delivery-level failure rather than silently acting on it.
- **Consequence if it does not land**: this gear must attribute callbacks by payload matching
  alone, which cannot distinguish a genuine confirmation from a replayed or forged one, and the
  PRD's requirement that system actors not be impersonable has no enforcement on the chosen
  transport.
- **Agreement status**: **UNASKED** — never registered upstream at any number.
- **Source**: PRD §6.3 (provisioning-intent confirmations), §9.1 (actor authenticity). Raised
  against `gears/bss/subscriptions/docs/SEAMS.md`, which has no such entry.

### 2.2 Payments

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-upreq-payment-authorization-outcome`

An **authorization outcome** for a payer, distinguishing **authorized**, **pending**, and
**failed** as three separate answers, consumed as a begin-fulfillment guard input (PRD §6.3,
`OrderAcceptanceRecorded` re-evaluation) and never stored as an order fact.

- **Owning upstream gear**: none. **Payments has no specification and no register anywhere in
  this repository**, so this ask has no owner to receive it. It is recorded here, following the
  precedent the sibling Lifecycle register set, for whichever specification eventually takes it.
- **Why this gear cannot satisfy it alone**: authorization is a payment-instrument concern outside
  this gear's domain; Orders Workflow only consumes the outcome as a precondition.
- **Consequence if it does not land**: only one payment ordering is expressible in this design —
  provision first, collect after, with authorization used as a risk check — which does not cover a
  self-service card checkout that must collect payment **before** provisioning.
- **Agreement status**: UNASKED — no specification exists to register it against.
- **Source**: PRD §6.3 (begin-fulfillment precondition), §13 Dependencies (Payments, `p1`).

### 2.3 Generic Approval

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-upreq-generic-approval-expectations-contract`

A canonical Generic Approval service satisfying the expectations contract this gear's PRD §9.2
already defines: accept an `OrderApprovalRequest` (order context including the named TCV figure,
gate identifier, idempotency key, `correlationId`); evaluate the approval requirement and
thresholds, supporting multi-party gates; accept and route the escalation command Workflow issues
on timer expiry; return an `OrderApprovalDecision` (approved/rejected, with reason, echoing
`correlationId`); be idempotent by request key; and answer the approval-**requirement** verdict
query keyed on `orderId` + `orderVersion`, cacheable per version. Since `ADR/0013` one further
clause is asked: the decision **event** carries references only — `orderId`, `orderVersion`, the
gate identifier and a `decisionEventId` — and the decision itself (outcome, reason, deciding
authority, deciding subject) is **read** by `record-decision` from the service by
`decisionEventId`, so no approver identity or reason text crosses into engine history
(`design/03-approval-execution.md` §3.3). Since D-189 two more are asked. "Idempotent by request
key" holds **for the life of the gate** — until the request is decided or cancelled — not for a
request-cache window, because the approval-request key
(`resource_tenant_id` + `orderId` + `orderVersion` + `gateId`) carries no attempt and is presented
again by a re-run after this gear's own step key aged out (30 days, D-185). And the service answers
a **read of a request by its request key**, which `open-gates` makes before it submits and so
adopts a request an unsettled earlier call made (`design/03-approval-execution.md` §3.6
`inst-og-submit`) — the confirm-by-lookup rule this gear already asks of Subscriptions after a key
ages out (§2.1 `SUB-O13`). The read **answers for every state of the request** — `open`,
decided or `cancelled` — never "not found" for a request it holds, and carries the request's state
and the instant it opened, from which `open-gates` dates an adopted gate's escalation window. For
a request decided before it is adopted, the service re-publishes its decision event under the same
`decisionEventId` until `record-decision` reads that decision by `decisionEventId`, so the
decision reaches this gear's `listen` like any other (D-189 as clarified).

- **Owning upstream gear**: Generic Approval service. **No canonical specification exists today**
  (the `gears/approval-service` gear PRD remains a stub); PRD §9.2 is the normative interface until
  one exists.
- **Why this gear cannot satisfy it alone**: approval routing and threshold configuration are
  policy-owner concerns this gear must not compute (Lifecycle R2); Orders Workflow is a consumer of
  the verdict, not its author.
- **Consequence if it does not land**: multi-party gates, escalation, the Approver Inbox, and PRD
  acceptance criteria #1-#4a (including #2a) remain inert. In the interim, the **phase-1 stand-in**
  behind the §9.2 contract is the deciding authority: it always returns `approval not required`
  (audited as stand-in), so all orders proceed as if approval were never required until the real
  service lands.
- **Agreement status**: UNASKED — no canonical specification exists to register this contract
  against; PRD §9.2 is a self-declared expectations contract, not an upstream-agreed one.
- **Source**: PRD §9.2 (External Integration Contracts), §6.2 (approval execution), §13
  Dependencies (Generic Approval service, `p1`); `DECISIONS.md` D-189 (request-key lifetime and
  read by request key).

### 2.4 Orders Lifecycle

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-upreq-submitted-ttl-visibility`

Orders Lifecycle **MUST** make the point at which a `submitted` order expires knowable to this
gear. **The preferred form is the per-order expiry instant**, not the policy value: Lifecycle
stamps `submitted_expires_at` on the order and echoes it on the `OrderSubmitted` event this gear
already consumes. A published configuration value for the TTL duration, or a contract-documented
constant, are acceptable fallbacks.

The per-order instant is preferred for three reasons. It needs no new read path, because the event
already crosses the boundary. It is immune to the policy being retuned after an order was
submitted — an in-flight order carries its own deadline rather than inheriting a value that may
have changed underneath it, and `orders_state_ttl_policy` is a mutable policy table. And it turns
this gear's escalation check from a startup assertion against a global into a per-order
computation, which is strictly more correct: two orders submitted either side of a policy change
have different deadlines and should escalate at different times.

- **Owning upstream gear**: Orders Lifecycle (`gears/bss/orders-lifecycle`).
- **Why this gear cannot satisfy it alone**: Lifecycle owns the order and its state machine. This
  gear acts on the order without owning it and cannot read, derive or safely assume the TTL.
- **Rationale**: this design carries a normative **MUST** — when an approval verdict cannot be
  obtained, the process parks and escalation **MUST** fire before the `submitted` TTL elapses
  (`ADR/0007`). Both the escalation threshold and the lead time before expiry are deliberately
  stated as functions of that TTL rather than as absolute numbers, precisely because a number
  fixed on this side would assume a value this gear cannot see. The relational form is only
  implementable if the TTL is readable.
- **Consequence if it does not land**: the MUST has no path to satisfiability. The assertion this
  design requires — that the escalation lead time is present, positive, and strictly less than the
  remaining time before expiry — cannot be evaluated, so the escalation path is configured but
  never operating, and a parked order's only remaining bound is the TTL expiring with no prior
  alert. A commercial order is then lost to an outage in a third service.
- **Note — the upstream design already states the reciprocal constraint**: Orders Lifecycle's
  `design/07-hold-and-expiry.md:518` records that its `submitted` TTL "must exceed the sibling
  gear's escalation lead time, since expiry bounds the fail-closed park". Both gears have
  independently written down the same inequality from opposite sides, and neither can currently
  evaluate it, because the value sits in one gear's policy table and the consumer is in the other.
  This ask is the wire between two halves that already agree.
- **Interim in place — this ask is open but not blocking**: `DECISIONS.md` D-57 mirrors the TTL as
  local configuration under a startup refusal (absent, zero, negative or mis-ordered values are
  refused at configuration load). The escalation path is therefore operable today. What the mirror
  cannot do is notice that Lifecycle has retuned the real value, which is the residual risk this
  ask closes and the reason it stays open.
- **Agreement status**: **UNASKED** — no Orders Lifecycle ask has previously been registered by
  this gear at any number.
- **Source**: `ADR/0007` (`cpt-cf-bss-orders-workflow-adr-fail-closed-verdict-park`); `DECISIONS.md`
  D-46 and Q-02. Raised against `gears/bss/orders-lifecycle/docs/UPSTREAM_REQS.md`.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-upreq-lifecycle-thin-events`

Orders Lifecycle **MUST** either publish, for each state event this gear consumes through the
platform, a **thin variant** that carries only the envelope, the event type, `orderId`,
`orderVersion`, `category` on `OrderSubmitted` (the start filter) and `supersedesVersion` on
`OrderAmended`, **or confirm** that the full events as published may be stored in the platform
engine's history for the tenants it serves. This gear reads everything else from Lifecycle under
the PDP inside its step operations (ADR-0013), so a thin variant loses it nothing.

- **Owning upstream gear**: Orders Lifecycle (`gears/bss/orders-lifecycle`).
- **Why this gear cannot satisfy it alone**: Lifecycle owns the event schemas. The events as
  published carry commercial data — `OrderSubmitted` the tenant axes, per-line references and pins
  and the resolved total's per-line net components; `OrderApproved` and `OrderRejected` the
  deciding authority; `OrderHeld` the hold reason; `OrderCancelled` the cancelling actor and the
  cancel reason; `OrderCompleted` the line-to-subscription mapping
  ([Lifecycle `01 §4.4`](../../orders-lifecycle/docs/design/01-foundation.md#44-events-audit-and-the-outbox-normative),
  lines 2396–2406) — and under ADR-0011 the platform, not this gear, consumes them: the start
  trigger's input and every `listen` output are engine data (Serverless Workflow DSL 1.0.0, dsl.md
  *Runtime expression arguments*; dsl-reference.md *Listen*).
- **Rationale**: this is the second of the two routes of ADR-0013 *Trigger inputs and consumed
  events*; the first is `…-upreq-serverless-runtime-consumed-event-member-storage` (§2.9). Either
  one closes the residual; a Lifecycle confirmation closes it by accepting it on the data owner's
  authority rather than by removing it.
- **Consequence if it does not land** (and the §2.9 route does not either): Lifecycle's commercial
  event fields sit in Temporal history under the platform's retention and authorization, and the
  "commercial data in engine history" threat of `DESIGN.md` §4.2 keeps that residual.
- **Agreement status**: **UNASKED**.
- **Source**: `ADR/0013`; `DECISIONS.md` D-66 (as amended); `DESIGN.md` §4.2. Raised against
  `gears/bss/orders-lifecycle/docs/UPSTREAM_REQS.md`.

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-workflow-upreq-lifecycle-failure-reason-coverage`

Orders Lifecycle **MUST** add two values to the closed `failure_reason` enumeration of
`acknowledge-failed` ([Lifecycle `06 §4.4`](../../orders-lifecycle/docs/design/06-workflow-seam.md#44-acknowledgement-normative),
its D-136), or name the existing value this gear should send for each:

| Proposed value | Raised when | This gear's reason |
|----------------|-------------|--------------------|
| `payment-authorization-stale` | the pre-activation re-check found the payment authorization older than its validity, so Workflow voided the wave-1 drafts before any activation | `payment-authorization-stale` (`design/04-fulfillment-plan.md` §3.6 `inst-rc-if-stale`) |
| `dependency-topology-unavailable` | Catalog could not return a complete, revision-stamped dependency topology for the plan, a plan task was exhausted, and Workflow halted before any subscription was created | `catalog-topology-unavailable` (`design/04-fulfillment-plan.md` §4.3) |

- **Owning upstream gear**: Orders Lifecycle (`gears/bss/orders-lifecycle`).
- **Why this gear cannot satisfy it alone**: Lifecycle owns the enumeration and the
  `OrderFulfillmentFailed` schema, and refuses any other value `request-invalid` at its boundary;
  its own text says adding a value is a contract change to the table and the event schema.
- **Rationale**: the fence maps every failure cause it admits to one Lifecycle value
  (`design/06-saga-and-compensation.md` §4.8). Seven causes have a faithful value; these two do
  not.
- **What the design cannot do until it lands**: acknowledge these two failures with an honest
  reason. Until then the fence sends `line-execution-failed` for a stale authorization and
  `dependency-graph-invalid` for an unavailable topology. The exact reason stays in
  `owf_cancellation_fence.orders_failure_reason` and in `OrderFulfillmentAborted`, so only
  Lifecycle's audit `caller_reason` and `OrderFulfillmentFailed` carry the approximation.
- **Agreement status**: **UNASKED**.
- **Source**: `DECISIONS.md` D-110. Raised against `gears/bss/orders-lifecycle/docs/UPSTREAM_REQS.md`.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-upreq-lifecycle-workflow-key-retention`

Orders Lifecycle **MUST** either **retain the idempotency records of the workflow-trigger class**
(the triggers of the five seam operations, [Lifecycle `01 §4.1`](../../orders-lifecycle/docs/design/01-foundation.md#41-the-transition-contract-normative)),
scoped to the Workflow service principal, for **at least 30 days** from the record's creation —
this gear's key lifetime (`design/01-foundation.md` §3.7 *Key lifetime*, D-185), after which it
never re-issues the same key — **or expose a read of a seam operation's stored outcome by its
idempotency key**, returning the outcome the key settled (committed, or the refusal) or "no
record".

- **Owning upstream gear**: Orders Lifecycle (`gears/bss/orders-lifecycle`).
- **Why this gear cannot satisfy it alone**: Lifecycle owns the registry and its 24-hour window
  ([Lifecycle `01 §2.2`](../../orders-lifecycle/docs/design/01-foundation.md#the-idempotency-window-is-24-hours-and-is-not-a-commercial-bound)).
  This gear re-issues an unchanged Lifecycle key after a lease death or an interruption that can
  last days — a hold, a ceiling park, an `invocation-dead` task awaiting its re-drive — and the
  keys of `begin-fulfillment`, `report-spawn-signal` and `report-outcome` carry no attempt, so
  even an operator-minted successor presents the same Lifecycle key.
- **Rationale**: Lifecycle's own constraint already states the requirement: "Past the window a
  replayed key is a new operation, so the window must exceed the longest caller retry horizon"
  (Lifecycle `01-foundation.md:238-241`). For this caller that horizon is 30 days. Past the window,
  a re-run of a transition that committed is refused `not-admissible` — the version check passes
  and the state table has no row from the state already reached (Lifecycle
  `06-workflow-seam.md:352-356`) — instead of replaying the stored success.
- **Interim in place — this ask is open but not blocking**: D-188 reads the order back on a
  `not-admissible` and, where it is at the call's version in the transition's target state,
  settles `already-applied` (`design/01-foundation.md` §3.3 *Rounds and attempts* rule 4). The
  residuals are what the read cannot show: the recorded verdict, failure reason and cancelling
  path are not in Lifecycle's order read, so a `cancelled` order is reported `terminal-event`
  rather than as this gear's cancel, and each such re-run appends one refused attempt to
  Lifecycle's audit trail.
- **Agreement status**: **UNASKED**.
- **Source**: `DECISIONS.md` D-188, D-185. Raised against
  `gears/bss/orders-lifecycle/docs/UPSTREAM_REQS.md`.

### 2.5 Catalog

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-upreq-catalog-dependency-topology-read`

Catalog **MUST** expose a read contract returning, for the product/offer references on an order's
lines, the **dependency topology** between them: which line's provisioning must precede which, the
cardinality of each edge, whether a bundle resolves to one provisioning unit or several, and a
stable identifier per node so the resolved topology can be frozen against an `orderId` +
`orderVersion` and replayed identically. The read **MUST** be versioned, so a topology change
cannot retroactively alter a frozen plan.

- **Owning upstream gear**: Catalog. It is not a PRD-registered actor for this gear, which is part
  of the gap: the fulfillment-plan slice already flags the dependency as real while the register
  named it nowhere.
- **Why this gear cannot satisfy it alone**: product structure and dependency relationships are
  Catalog-owned facts. This gear can neither invent them nor derive them from the order document,
  which carries line items, not their provisioning order.
- **Rationale**: plan construction, cycle validation, wave assignment and the reverse-order
  compensation walk are all defined over this topology. Without a contract the design depends on
  an undocumented read whose shape, stability and versioning are unknown, and a frozen plan cannot
  be shown to be reproducible.
- **Consequence if it does not land**: the fulfillment plan is constructed against an unspecified
  interface, dependency cycles cannot be rejected at construction time with any guarantee, and a
  mid-flight Catalog change can silently alter the topology a frozen plan was built from —
  producing a compensation walk that unwinds in an order the forward run never used.
- **Agreement status**: **UNASKED** — Catalog appears nowhere in this register today and has no
  seam map this gear can register against.
- **Source**: PRD §6.1 (Fulfillment Plan Construction); `DECISIONS.md` D-16;
  `design/04-fulfillment-plan.md` §3.6.

### 2.6 Privacy and data classification

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-upreq-pii-classification-ruling`

The PRD owner, with the privacy owner, **MUST** replace the PRD's "Privacy / PII: not applicable"
exclusion with a positive data-classification ruling covering the identifiers this design actually
retains, and **MUST** state the lawful basis under which a ≥ 400-day audit retention answers an
erasure request.

- **Owning upstream gear**: none — this is a **PRD amendment** and a privacy-owner ruling, out of
  scope for the design set. It is recorded here rather than resolved in the design, per the
  standing rule that the PRD is approved input.
- **Why this gear cannot satisfy it alone**: a data-classification and lawful-basis determination
  is a privacy-owner decision; a design that made it for itself would be asserting a legal
  position, not recording one.
- **Rationale**: the PRD excludes PII "beyond tenant and actor identifiers carried in the audit
  trail" — a carve-out whose exception swallows it. This design writes `owf_audit_entry.actor` on
  100 % of process transitions and retains it for at least 400 days, records operator identity and
  free-text justification on every override, and names the human `deciding_authority` who approved
  a commercial decision. That is personal data under any ordinary reading, held for over a year,
  under an append-only no-delete constraint this design deliberately hardened (`DECISIONS.md`
  D-50, D-59). The actor itself is an opaque `SecurityContext` subject UUID, never a name or an
  email (D-61), and the identity-lifecycle guarantees that reference relies on — stability,
  non-reuse, deletion across caches and backups — are the shared p2 platform follow-up Lifecycle
  registered as `cpt-cf-bss-orders-lifecycle-upreq-audit-identity-lifecycle`
  ([Lifecycle `UPSTREAM_REQS.md` §2.8](../../orders-lifecycle/docs/UPSTREAM_REQS.md#28-identity-platform)),
  which this gear cites rather than duplicates; pseudonymisation is not anonymisation, so this
  ruling is still required.
- **Consequence if it does not land**: the gear's retention posture and its erasure posture are in
  unresolved conflict, and the conflict is discovered at the first erasure request rather than at
  design time. The append-only, hash-chained audit store has no compliant deletion path by
  construction — and no in-place rewrite path either, since erasure changes identity data held by
  the identity platform, never the audit row (`DESIGN.md` §4.3, D-61) — so any answer that requires
  deleting or rewriting rows would invalidate the design rather than configure it.
- **Agreement status**: **UNASKED** — no privacy owner has been named and no ruling requested.
- **Source**: PRD §8 (out-of-scope / privacy exclusion); `DECISIONS.md` D-50, D-38;
  `design/01-foundation.md` §3.7 (`owf_audit_entry`).

### 2.7 Event Broker

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-upreq-event-broker-shared-prerequisites`

Orders Workflow publishes its six process events through the platform producer outbox
(`event-broker-sdk::DbProducer` over `toolkit_db::outbox`, `ADR/0008`, `DECISIONS.md` D-58),
exactly as Orders Lifecycle does. The platform prerequisites that decision depends on are already
registered, in full, by the sibling gear in
[`gears/bss/orders-lifecycle/docs/UPSTREAM_REQS.md` §2.7](../../orders-lifecycle/docs/UPSTREAM_REQS.md#27-event-broker),
and this gear **co-signs them by reference rather than restating them**:

- `cpt-cf-bss-orders-lifecycle-upreq-event-broker-runtime` — a production `EventBrokerApi`
  runtime through `ClientHub` supporting the managed chained producer protocol, with the deployed
  topic partition count published;
- `cpt-cf-bss-orders-lifecycle-upreq-event-broker-cursor-retry` — SDK cursor-recovery and retry
  semantics for the managed chained producer;
- `cpt-cf-bss-orders-lifecycle-upreq-event-broker-dead-letter-recovery` — a shared operator
  interface and SDK republication that preserves event ID and business payload, so no gear needs
  a REST re-drive wrapper;
- `cpt-cf-bss-orders-lifecycle-upreq-event-broker-root-tenancy` — the authoritative
  platform-root tenant UUID source and broker authorization behaviour for root-tagged events;
- `cpt-cf-bss-orders-lifecycle-upreq-event-delivery-observability` — producer-queue depth, age,
  lag and dead-letter measurements exposed per queue.

Each of those asks **MUST** be satisfied for the `bss-orders-workflow-events` producer queue and
the `gts.cf.bss.orders_workflow.*` event family on the same terms as for Lifecycle's queue; this
gear raises no divergent requirement other than the grants for its own topic below (D-177), and
accepts whatever resolution the sibling register records.

**This gear's own topic's grants.** The six events publish on this gear's own topic,
`gts.cf.core.events.topic.v1~cf.bss._.orders_workflow.v1`, not on Lifecycle's
(`design/01-foundation.md` §4.7, `DECISIONS.md` D-177), so the grants the root-tenancy ask names
for Lifecycle's topic do not cover it and **MUST** be provisioned for it separately, on the terms
Lifecycle `design/08-read-and-authz.md` *Internal lifecycle stream authorization* states for its
own stream: the Workflow producer service principal **MUST** have explicit broker authorization
to publish the `gts.cf.bss.orders_workflow.*` family on this topic under platform-root tenancy,
and no other principal may produce on it; each consumer service principal **MUST** have explicit
authorization for the event types it requires, this topic and its consumer group, with
root-scoped event access; membership of the root tenant grants neither. The deployed broker
partition count for this topic **MUST** be published as the runtime ask requires for Lifecycle's.
Registering the topic instance itself is this gear's, at init before readiness, and is not asked
of the platform.

- **Owning upstream gear**: `event-broker` (`gears/system/event-broker`) and the platform SDK/GTS
  guideline maintainers, as named in the Lifecycle register.
- **Why this gear cannot satisfy it alone**: the runtime, the SDK recovery path, the operator
  tooling, the root-tenant identity source and the queue metrics are platform capabilities;
  building any of them here would recreate the gear-owned outbox D-58 removed.
- **Consequence if they do not land**: until the runtime lands, this gear **MUST NOT** report
  ready for event-producing traffic (`DESIGN.md` §3.5) and can test only against an
  `EventBrokerApi` double. Until dead-letter recovery lands, a platform dead letter on the
  Workflow queue has no supported republication path and the gap it leaves is permanent for that
  event; a producer-registration rotation is its expected bulk case, rejecting every event
still queued under the old producer id at once (`design/01-foundation.md` §3.7, D-178). Until
the grants on this gear's topic are provisioned, no event it enqueues can be published or
consumed. Until root tenancy is confirmed, the envelope `tenant_id` contract of
  `design/01-foundation.md` §4.7 is stated but unverified. Until delivery observability lands,
  the p95 < 30 s target (`cpt-cf-bss-orders-workflow-nfr-owf-event-latency`) has no producer-queue
  lag metric to be measured by and Q-07's resolution is unevidenced.
- **Agreement status**: **REGISTERED by Lifecycle, open**; this gear adds a co-signature and the
  grant line for its own topic above (D-177), not a new capability.
- **Source**: `ADR/0008` (`cpt-cf-bss-orders-workflow-adr-outbox-process-events`); `DECISIONS.md`
  D-58, D-177, D-178, Q-07; `design/01-foundation.md` §3.6, §3.7, §4.7; `DESIGN.md` §3.5, §4.4, §4.5.

### 2.8 Platform authorization policy

**Withdrawn ask.** An earlier revision carried `AUTH-O1`, a request that the platform auth
gateway resolve an approver principal to its assigned gate set. D-63 withdraws it: approver
assignment is a property of the gate row (`assigned_principal`, `design/03-approval-execution.md`
§3.7) evaluated by the PDP as an own-resource constraint, so no token claim and no gateway
capability is needed. Nothing replaces it here; the provisioning of that grant is item 1 below.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-upreq-pdp-policy-integration`

**Owner**: platform authorization and deployment owners — the `authz-resolver` PDP provider and
its policy and role provisioning. Mirrors and should be provisioned together with Orders
Lifecycle's `cpt-cf-bss-orders-lifecycle-upreq-pdp-policy-integration`
(`gears/bss/orders-lifecycle/docs/UPSTREAM_REQS.md` §2.9).

**Ownership boundary: follow Lifecycle's integration pattern, which is Pricing's.** Workflow
declares its resource/action catalogue and trusted property inputs, requests decisions through
`authz-resolver-sdk` `PolicyEnforcer`, and enforces returned scopes and business guards. Platform
owners select and operate the PDP provider, provision policies and role assignments, and supply the
tenant relationships those policies evaluate. Workflow builds no parallel evaluator and infers no
permission from successful registration of an `AuthzPermissionV1` instance. The authoritative
Workflow policy contract is `design/09-read-and-authz.md` §3.1 (catalogue), §3.2 (endpoint
mapping) and §4.1 (expected decisions); the bounded worker exception there is separate and exempts
no REST, SDK or event-handler caller.

**The concrete ask:**

1. **Catalogue registration.** Register the resource labels
   `gts.cf.bss.orders_workflow.{process_instance,process_step,fulfillment_task,manual_task,approval_gate,progress,audit}.v1~`
   and their permission instances `…cf.bss.orders_workflow.<resource>_<action>.v1` for the
   actions of `09 §3.1` — `process_instance × {cancel, retry_step}` (`start` is retired with the
   REST start route, D-73), `process_step × execute` with its thirty-five `operation` values as
   the property domain, `fulfillment_task × read`, `manual_task × {read, resolve, override,
   assign, escalate, cancel}`, `approval_gate × {read_inbox, approve}`, `progress × read`, `audit × read` (D-186) — with the
   `supported_properties` each `ResourceType` advertises. The `dead_letter` label and its `read`,
   `redrive` and `discard` actions are **not registered** until the platform answers
   `…-upreq-serverless-runtime-dead-letter-operator-visibility` (§2.9).
2. **Roles.** Provision Approver, Fulfillment Operator and Seller Operator roles producing the
   expected decisions of `09 §4.1`, with seller scope as a constraint on `seller_tenant_id` and
   tenant isolation as a constraint on `resource_tenant_id`; Fulfillment Operator holds no
   `process_instance × cancel`, `manual_task × cancel`, `dead_letter × discard` or `audit × read`,
   and only the Seller Operator holds `audit × read` (D-186).
3. **The approver grant keyed on `assigned_principal`.** `approval_gate × read_inbox` and
   `approval_gate × approve` granted as an "own resource" constraint
   `assigned_principal = subject_id` (an `Eq` on a custom property this gear supplies), and
   `progress × read` / `fulfillment_task × read` for an approver constrained to the orders
   carrying such a gate. This replaces the former gateway ask (an inverse principal-to-gate query): no token claim and no
   assignment-directory inverse query is needed, because the assignment is a column on the gate.
4. **Service-principal grants.** Confirm the service-subject `subject_type` identifier and the
   token scope that names this gear; grant the **serverless-runtime** service subject
   `process_step × execute` for the thirty-three enumerated `operation` values of `09 §3.1` (never
   `settle-from-lookup` or `retry-step`, which run only in-process, D-108; never the action unconditionally), restricted to the call's
   `correlationId` within its `resource_tenant_id`, and nothing else; grant Orders Lifecycle's
   service subject `progress × read` and `fulfillment_task × read` with a resource-id constraint
   to the named order (no `start`: the REST start route is removed, D-73); confirm that no service
   subject holds a pair that writes order state.
5. **Subject-only evaluation for the apply-time re-check** (`09 §4.4`): a decision on a context
   carrying `subject_id`, `subject_type`, `subject_tenant_id` and `token_scopes` but no bearer
   token, days after acceptance. If the provider cannot evaluate it, say so; the re-check then
   fails closed: the cancel is refused before the fence, and an `authority-withdrawn` manual task
   holds it after (D-115).
6. **Delegation proof as request context.** Lifecycle D-111 items 1–3 (carrier, policy
   evaluation, distinguishable deny reasons) apply by reference; Workflow never validates proof.

**Acceptance evidence:** identify the deployed provider and policy/configuration revisions, then
jointly test: principals in the same seller scope with different action grants; an approver not
assigned to a gate receives 404 on the decision endpoint and an empty inbox page; the submitting
identity receives `submitter-barred`; a Fulfillment Operator's cancel receives `not-authorized`
(403) while an operator outside the seller scope receives `not-found` (404); a service subject on
a human-actor arm is refused; a revoked grant is reflected at the apply-time re-check; PDP outage
returns 503 with no mutation and no idempotency-key settlement while the workers continue; and
the CI conformance test's recorded questions match the provisioned policy. Workflow owns the
integration tests and correct request/scope handling; platform owners own provider policy
behaviour and provisioning.

**Consequence if it does not land:** no caller-driven operation is authorizable in production —
the design fails closed on a missing grant rather than fabricating one — and the approver inbox
in particular cannot list a single gate. `PRD.md:320`'s "requests outside the approver's scope
MUST NOT be shown" is enforceable by construction once item 3 lands and not before.

**Status:** **UNASKED** — never registered against the platform authorization owner; open
production-integration verification, not proof that a new platform feature is required. Reuse
existing provider and toolkit capabilities first; the one item that may need an extension is
item 5, and it is asked as a question, not a requirement. Record the responsible platform owner
and evidence before production use of affected operations.

### 2.9 Serverless runtime (platform durable execution)

Under `ADR/0011` the order process flow is a versioned Serverless Workflow definition registered in
the platform gear `serverless-runtime` and executed by its Temporal plugin; this gear provides the
step operations and the process record (`DECISIONS.md` D-65…D-69). Every ask below is a platform
capability the design depends on that **no serverless-runtime document states as a fact** today;
each cites the platform text it builds on, by file and line, and none invents an API beyond the
platform's own [DESIGN.md §3.3](../../../serverless-runtime/docs/DESIGN.md#33-api-contracts).
`gears/serverless-runtime/` holds documentation and a `gear.toml` and no crate, so the whole
section is also the content of the readiness gate, except the re-drive clause of
`…-upreq-serverless-runtime-signals`, which gates the `invocation-dead` task's `retry` and not
readiness (`design/01-foundation.md` §3.8, D-192).

- **Owning upstream gear (all asks in this section)**: `serverless-runtime`
  (`gears/serverless-runtime/docs/`), its host, SDK and Temporal plugin owners.
- **Agreement status (all asks in this section)**: **UNASKED** — never registered against the
  serverless-runtime design; the platform's own open-scope list
  ([NEXT_ADR_SCOPE.md](../../../serverless-runtime/docs/NEXT_ADR_SCOPE.md)) names several of the
  gaps without a resolution.
- **Why this gear cannot satisfy them alone**: the definition's execution — durability, timers,
  retry, event matching, signals, history — is the plugin's by the platform's own decision
  ([serverless-runtime ADR-0005](../../../serverless-runtime/docs/ADR/0005-cpt-cf-serverless-runtime-adr-thin-host.md));
  building any of it here would be the second orchestration engine ADR-0011 rejects.
- **The fallback property**: if any `p1` ask below is declined, the process record and the step
  operations are unchanged and only sequencing falls back to code
  (`owf_definition_binding.definition_source = code`, `DESIGN.md` §4.9); the asks gate the
  platform path, not the record.

#### Platform delivery and readiness

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-upreq-serverless-runtime-readiness-gate`

serverless-runtime **MUST** deliver its host, the Function Registry, the Invocation and Event
Trigger APIs and the Temporal plugin to a state this gear can report ready against: the registered
definition version this gear binds new instances to resolves in the registry and the invocation
API answers (`design/01-foundation.md` §3.8). The suspension window is not asked: the Workflow
callable declares `workflow_traits.max_suspension_days: 90` (`design/10-process-definition.md`
§3.1), the required field of the platform schema whose default is 30
([DESIGN_GTS_SCHEMAS.md](../../../serverless-runtime/docs/DESIGN_GTS_SCHEMAS.md) lines 520–529),
because a suspension that outlives it moves the invocation `suspended → failed`
([DESIGN.md](../../../serverless-runtime/docs/DESIGN.md) line 455). What is asked is only
**whether `max_suspension_days` measures one suspension or the suspended time accumulated over the
invocation** — the re-check loops of `design/10-process-definition.md` §3.6 wake the invocation
at least hourly, so a per-suspension measure is never approached, whereas a cumulative measure
would reach 90 days on a long-lived instance — and **whether a tenant runtime policy may cap that
value below 90** — the platform commits to
suspension of at least 30 days with a tenant-configurable maximum
([serverless-runtime PRD.md](../../../serverless-runtime/docs/PRD.md) line 404; open as BR-009,
[NEXT_ADR_SCOPE.md](../../../serverless-runtime/docs/NEXT_ADR_SCOPE.md) line 21) — and, if it may,
that this gear's tenants are provisioned with a cap of at least 90 days, the `max_process_lifetime`
of D-53. Delivery further includes (a) **confirmation that `traits.invocation: { supported:
[async], default: async }` is the async-only declaration** DESIGN.md line 653 asks of a Workflow
that suspends: the platform text says `workflow_traits` SHOULD declare it, but the Workflow base
type's required `traits.invocation` (DESIGN_GTS_SCHEMAS.md lines 1139–1159) already expresses
it, and the callable declares it (`design/10-process-definition.md` §3.1, decision D-127);
(a′) **the meaning of the declared limits**: whether `traits.limits.timeout_seconds` (declared
23,328,000, 270 days) measures wall-clock time including suspension, whether
`traits.limits.max_concurrent` (declared 50,000) counts suspended invocations — the value to use
under each answer is stated in `design/10-process-definition.md` §3.1 (decision D-157), and the
answer gates readiness — whether the platform has capacity for them above its stated regional
target of 10,000 concurrent executions (serverless-runtime PRD.md line 843), and that this gear's
tenants are provisioned with `max_execution_duration_seconds`, `max_concurrent_executions` and
`max_execution_history_mb` (DESIGN_GTS_SCHEMAS.md lines 1824–1855) that admit them — the schema
defaults (30 s, 100) would end or throttle every order, and the platform's duration guardrail
applies "even if higher timeouts are requested" (serverless-runtime PRD.md line 483); and
(b) an answer to the DSL expressiveness questions of `DECISIONS.md` Q-11 as the plugin implements
them (whether it accepts a runtime-expression `wait` duration as an extension — Serverless
Workflow DSL 1.0.0 admits only an inline duration object or an ISO 8601 string (dsl-reference.md,
*Wait* and *Duration*), so until then the definition arms its computed waits as the bounded
re-check loop of D-70 as amended; `error_code` visible on `$error`; a dynamic parallel construct;
a cancellable `listen` inside a competing `fork` without event loss; the hold pattern without a
Function; whether a `catch` that carries only `retry` re-raises the last error once its limit is
spent, Q-11 (vi), which every retry-only `catch` of the canonical definition relies on). An aggregate retry cap, the gear-wide 10 % retry budget of D-43, is asked as a
platform-side property of the plugin's execution of the definition's task retries (`use.retries`)
rather than rebuilt here.

- **What the design cannot do until it lands**: nothing on the platform path runs — the canonical
  definitions of `design/10-process-definition.md` are documentation, the event triggers are not
  enabled, and this gear **MUST NOT** report ready for the `platform` definition source. A
  tenant cap on `max_suspension_days` below 90 would move a held or long-running invocation
  `suspended → failed` before the lifetime ceiling could park it, so such a cap fails the gate; a
  cumulative measure would do the same to any instance that lives near 90 days, including one
  unparked under a fresh ceiling.
- **Source**: `ADR/0011` (*Runtime gate*); `design/01-foundation.md` §3.8; `design/10-process-definition.md` §1.1, §3.1, §4.5;
  `DECISIONS.md` D-70 (as amended), Q-11; serverless-runtime [ADR-0004](../../../serverless-runtime/docs/ADR/0004-cpt-cf-serverless-runtime-adr-temporal-workflow-engine.md),
  [ADR-0005](../../../serverless-runtime/docs/ADR/0005-cpt-cf-serverless-runtime-adr-thin-host.md) line 83 (plugin layout).

#### Event triggers over event-broker GTS events

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-upreq-serverless-runtime-event-triggers-gts`

The event-trigger path and the plugin's `listen` **MUST** consume event-broker GTS events — the
Orders Lifecycle state events, the Generic Approval decision event and the Subscriptions outcome
events — under **platform-root tenancy** (Lifecycle D-95) with **per-order ordering** preserved
(the broker partition key is `orderId`), correlate a `listen` on `orderId` and `orderVersion`, state **the input shape a trigger passes to
the invocation it starts** (the definition's `input.from` reads the event envelope — `.id`,
`.type`, `.data.<member>` — as that input, `design/10-process-definition.md` §3.6 (a), and no
platform document states whether a trigger passes the envelope, its `data` only, or a mapped
`params` object), and
support two start triggers (`OrderSubmitted` filtered to `category = new_sale`, and
`OrderAmended`) on one Workflow callable. One broker event **MUST** be able both to start an
invocation through a trigger and to reach a running invocation's `listen` (an `OrderAmended` for a
new version while the prior version's invocation is still unwinding). An event that correlates to
no running invocation **MUST** be handled on the platform trigger path, not dropped silently. What
the engine stores of a trigger input or a consumed event is the separate ask
`…-upreq-serverless-runtime-consumed-event-member-storage` below. The platform's event-broker integration is "TBD per deployment"
([serverless-runtime DESIGN.md](../../../serverless-runtime/docs/DESIGN.md) line 150) and event
matching is plugin-native (line 808).

- **What the design cannot do until it lands**: no instance starts on the platform path; the eight
  non-start Lifecycle triggers, the approval decision and the Subscriptions confirmations cannot
  reach a running instance, so the definition's arms fall back to its poll `wait`s where it has
  them and have no path at all where it does not (hold, resume, terminal events); supersession of
  an amended order cannot start the new version's invocation; the Subscriptions confirmation
  arm is dropped for the poll arm (D-97); and if a trigger passes a shape other than the event
  envelope, the definition's `input.from` must be rewritten to it before the first publish.
- **Source**: `design/02-triggers-and-start.md` §2.2, §4.7; `design/10-process-definition.md` §2.2 *The closed trigger set*, §3.3, §3.6 (f); `design/05-provisioning-intents.md` §4.1.

#### Member-only storage of trigger inputs and consumed events

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-upreq-serverless-runtime-consumed-event-member-storage`

For the start trigger's input and for every event a `listen` consumes, the plugin **MUST** persist
in engine history only the members the definition selects — the workflow's `input.from`, and a
`listen` task's `read` mode with its `output.as` and `export.as`, applied **before** the value is
written to history, not only to the in-memory workflow data — so that no raw event reaches the
Temporal persistence backend. The
Serverless Workflow DSL 1.0.0 exposes the raw workflow input as `$workflow.input` (dsl.md, *Runtime
expression arguments*), and the output of a `listen` is the array of consumed events (dsl-reference.md,
*Listen*); nothing in the platform states whether what the engine records is the value before or
after that filtering. The events as published carry commercial data ADR-0013 keeps out of history:
Lifecycle's `OrderSubmitted` carries the tenant axes, per-line references and pins and the resolved
total's per-line net components; `OrderApproved` and `OrderRejected` the deciding authority;
`OrderHeld` the hold reason; `OrderCancelled` the cancelling actor and the cancel reason;
`OrderCompleted` the line-to-subscription mapping
([Lifecycle `01 §4.4`](../../orders-lifecycle/docs/design/01-foundation.md#44-events-audit-and-the-outbox-normative),
lines 2396–2406); the Subscriptions outcome event carries a `subscriptionId`; the Generic Approval
decision event has no specification (§2.3, Q-05). The platform has no data-classification model
for execution history ([NEXT_ADR_SCOPE.md](../../../serverless-runtime/docs/NEXT_ADR_SCOPE.md)
line 23, BR-017).

- **What the design cannot do until it lands**: ADR-0013's "no commercial data in engine history"
  holds for task inputs and outputs only; the start trigger's input and every consumed Lifecycle,
  Generic Approval and Subscriptions event may sit in history in full, which is the stated residual
  of ADR-0013 and of the "commercial data in engine history" threat (`DESIGN.md` §4.2). The
  residual closes when this ask **or** the Lifecycle thin-event ask
  (`…-upreq-lifecycle-thin-events`, §2.4) together with the §2.3 reference-only decision event
  lands; until then PRD §15's first question is answered "none" for task data and "the consumed
  events, as published" for trigger inputs and `listen` outputs.
- **Source**: `ADR/0013` (*Trigger inputs and consumed events*); `DECISIONS.md` D-66 (as amended);
  `DESIGN.md` §4.2; `design/10-process-definition.md` §3.3.

#### PDP-guarded calls under a propagated service identity

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-upreq-serverless-runtime-pdp-guarded-call`

The plugin's HTTP `call` task **MUST** invoke `POST /bss-orders-workflow/v1/steps/{operation}` as
the **serverless-runtime service principal** — a gateway-asserted `SecurityContext` whose
`subject_type` is the platform service-subject type and whose `token_scopes` name this gear — so
the step route's principal check and the PDP's `process_step × execute` decision
(`design/09-read-and-authz.md` §3.1, §4.1) have a caller to decide on, and **MUST** carry the
invocation's `resource_tenant_id` as the tenant context of that principal's call. The ask is
narrowed to the **outbound** call. Which identity a triggered execution runs under is already a
trigger field — `execution_context: system | event_source`, default `system`, "platform identity"
([DESIGN_GTS_SCHEMAS.md](../../../serverless-runtime/docs/DESIGN_GTS_SCHEMAS.md) line 1697) — and
this gear's two start triggers take `system`, because an `event_source` identity would be
Lifecycle's producer principal, which the step route refuses. What no platform document states is
how that identity is presented on an HTTP `call` the plugin issues: the `subject_type`, the
`token_scopes` naming this gear, the tenant context, and the refresh of that credential across an
invocation that runs up to 90 days. The platform lists the execution-identity model and the
credential lifecycle of long-running workflows as open
([NEXT_ADR_SCOPE.md](../../../serverless-runtime/docs/NEXT_ADR_SCOPE.md) lines 15 and 96–97,
BR-006, BR-013).

- **What the design cannot do until it lands**: no step route can be authorized, because the only
  principal the policy may grant `execute` to has no defined identity on an outbound call; the
  design fails closed and no operation runs on the platform path.
- **Source**: `ADR/0011` (*Platform asks*); `ADR/0010` as amended; `design/01-foundation.md` §3.3 steps 1–2; `design/09-read-and-authz.md` §3.5.

#### Attempt identity and deadline propagation on every call

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-upreq-serverless-runtime-attempt-and-deadline-propagation`

Every HTTP `call` the plugin issues **MUST** carry, to the callee, the attempt that issued it and
the remaining deadline of the enclosing task timeout, and the platform **MUST** confirm that the
DSL's `$workflow.id` is the platform `invocation_id`. The call **MUST** also carry the invocation that issued it as a
platform-asserted value — one the callee can trust as the platform's statement, not a body member
the definition computes — because every step call after `start-instance` is bound to the
instance's invocation (`design/01-foundation.md` §3.3 step 3, `DECISIONS.md` D-106). The values exist inside the platform: the
SDK `Context` carries `invocation_id`, `attempt_number` (1-indexed, adapter-tracked) and a
`deadline` with `remaining_time()`
([serverless-sdk DESIGN.md](../../../serverless-runtime/serverless-sdk/docs/DESIGN.md) lines 123,
299–307). What is asked is only that they are **carried on the outbound HTTP call** — as request
headers the step envelope reads — because a step operation is an HTTP route, not an SDK handler,
and nothing states that a DSL `call: http` task transmits them. `$workflow.id` is the DSL's
"unique id of the workflow execution" (Serverless Workflow DSL 1.0.0, dsl.md, *Runtime expression
arguments*); that it equals the Invocation API's `invocation_id` is assumed by `start-instance`,
which binds `owf_process_instance.invocation_id` from it (`design/01-foundation.md` §3.3
*Attempt identity*), and by the definition's call envelope (`design/10-process-definition.md`
§3.6), and no platform document states it.

- **What the design cannot do until it lands**: `owf_step_log.attempt_id` records the stand-in
  `"{invocationId}:{taskReference}"` (the DSL's `$task.reference`, the step's position in the
  document) plus the key's receipt ordinal (`01 §3.3` *Attempt identity*) rather than the
  platform's attempt, so an auditor's join to the platform timeline is by task position rather
  than attempt; and the envelope bounds each attempt by the operation's own
  `deadline_ms` only, so an attempt started late in the task timeout can outlive it and be
  answered to a caller that has already given up (absorbed on the re-issue, but wasted). If
  `$workflow.id` is not the `invocation_id`, the binding holds an identifier no Invocation API call
  accepts, so the sweep's status read and the operator re-drive of D-86 address nothing. Until the
  invocation is asserted on the call, the binding compares the body's `invocationId`, so a
  principal holding the serverless-runtime scope that also learns an instance's invocation id
  (the progress read shows it to an operator) can present it.
- **Source**: `design/01-foundation.md` §3.3 step 4, *Attempt identity*, §4.5; `design/10-process-definition.md` §3.1.

#### Engine-history residency, retention and reference-only task inputs

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-upreq-serverless-runtime-history-residency-retention`

For a residency-bound tenant, the Temporal persistence backend holding this gear's invocation
history **MUST** be pinned to the tenant's jurisdiction with no cross-boundary replication, its
retention **MUST** be stated and bounded to the recovery window this gear needs (the 90-day
lifetime ceiling plus the sweep floor), and the platform **MUST** confirm that its timeline, debug
and trace endpoints expose task inputs and outputs only to principals the platform authorizes for
this gear's tenant. Task inputs and outputs are reference-only by this gear's own rule (ADR-0013);
what remains in history from them is the ADR-0013 vocabulary as amended by D-131: identities,
including `resource_tenant_id`, which is in every task input and `Idempotency-Key` header, so the
timeline shows which resource tenant owns each order; opaque record references; counters and
array cardinalities; and the fixed-member error answers of D-132;
trigger inputs and consumed events are reference-only only once
`…-upreq-serverless-runtime-consumed-event-member-storage` or the Lifecycle thin-event ask lands
(ADR-0013 *Trigger inputs and consumed events*). This ask is about where and for how long what
remains in history lives. Temporal Server's persistence is a
platform infrastructure dependency
([serverless-runtime ADR-0004](../../../serverless-runtime/docs/ADR/0004-cpt-cf-serverless-runtime-adr-temporal-workflow-engine.md)
line 100), retention is a `TenantRuntimePolicy` concern
([DESIGN.md](../../../serverless-runtime/docs/DESIGN.md) line 739), and the platform has no data
classification model for execution history
([NEXT_ADR_SCOPE.md](../../../serverless-runtime/docs/NEXT_ADR_SCOPE.md) line 23, BR-017).

- **What the design cannot do until it lands**: the PRD §15 evaluation of engine-history isolation,
  retention and residency cannot be completed, so Q-01 stays half-answered (`DECISIONS.md` Q-12);
  the platform path is not ready for a residency-bound tenant (`DESIGN.md` §2.2); and the
  "commercial data in engine history" threat stays *bounded, not closed* (`DESIGN.md` §4.2).
- **Source**: `ADR/0013` (*Residual*); `ADR/0011` (Q-01 part 2); `DESIGN.md` §2.2, §4.2, §4.3; PRD §15 Q-01.

#### Definition versioning, pinning and a pre-publish validation hook

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-upreq-serverless-runtime-definition-versioning-validation-hook`

The Function Registry **MUST** (1) call a **consumer-registered validation hook** before a Workflow
version is published, so this gear's ADR-0012 rules run on every publish and a refusal blocks it —
the platform surface today has only the plugin's registration-validation hook
([DESIGN.md](../../../serverless-runtime/docs/DESIGN.md) lines 762, 792); (2) keep a published
version **resolvable while any instance is bound to it**, refusing its `archived`/`deleted`
transition while a binding names it — the lifecycle hard-deletes after a retention period (lines
609–610); (3) pin an in-flight invocation to the version it started under, which the platform
already states (line 614, BR-029) and reports as `function_version` on the invocation record that
`start-instance` reads ([DESIGN_GTS_SCHEMAS.md](../../../serverless-runtime/docs/DESIGN_GTS_SCHEMAS.md)
*InvocationRecord*); (4) **audit every publish** with actor, version and validation result, and
report the publisher of each version on the registry's read of it, which `start-instance` copies
into `owf_definition_binding.published_by` — the platform's audit of definition changes is
unaddressed (NEXT_ADR_SCOPE.md line 26, BR-034), a registered callable carries no publisher
(`owner`, `created_at`, `updated_at` only, DESIGN_GTS_SCHEMAS.md line 23), and publishing
governance is unaddressed too (line 48, BR-122); and (5) until (1) lands, **restrict publishing**
of `order_process` versions, and their `deprecate`, `disable`, `archive` and `delete`
transitions, to the identity of this gear's definition publish job, so a version the job did not
validate cannot be published (decision D-137).

- **What the design cannot do until it lands**: the fence at publish time is only the CI
  conformance test, the behavioural gate and the publish job (`design/10-process-definition.md`
  §4.2), so a publish outside the job is unvalidated and caught only by the run-time guards —
  the binding still records the version the platform pinned, never one the document claims; a
  bound version the registry deletes leaves instances whose audit cites a definition nobody can
  read; and who published a version is evidenced only by the publish job's run and the registry's
  version listing, with `owf_definition_binding.published_by` null.
- **Source**: `ADR/0012`; `design/10-process-definition.md` §2.2, §3.2, §4.2, §4.3;
  `design/01-foundation.md` §3.3, §3.7; `DECISIONS.md` D-137.

#### Which version an event trigger starts, and a scoped activation

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-upreq-serverless-runtime-trigger-version-selection`

The platform **MUST** state which version of a Workflow an event trigger starts. A trigger binds a
`function_id` ([DESIGN_GTS_SCHEMAS.md](../../../serverless-runtime/docs/DESIGN_GTS_SCHEMAS.md)
*Trigger*), whose GTS id names only the major (`…order_process.v1~`), and the platform states only
that an invocation is pinned to "the exact version at start time"
([DESIGN.md](../../../serverless-runtime/docs/DESIGN.md) line 614) and that `active` and
`deprecated` versions are callable (line 924). The ask is (1) that a trigger starts the newest
`active` version of the major it names, or the version its binding pins, stated as one rule;
(2) that a binding **MAY** pin an exact version, so a version is switched on by re-pointing the
binding rather than by deprecating its predecessor; and (3) optionally, a binding scoped to a
tenant or a share of events, so a candidate version can start the orders of a canary seller
before all others.

- **What the design cannot do until it lands**: know which version new orders start on after a
  publish. The publish job therefore deprecates the replaced version in the same run, so that one
  version per major is `active`, and the readiness check alerts on a new binding to any other
  version (`design/10-process-definition.md` §3.8, §4.2). No canary exists: the behavioural gate
  in a non-production environment is the only run of a candidate before production, and a bad
  version reaches every new order in the environment until the job publishes the last good
  document as a new version.
- **Source**: `design/10-process-definition.md` §4.2; `DECISIONS.md` D-138.

#### Named signals and invocation control

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-upreq-serverless-runtime-signals`

The Invocation API **MUST** deliver a **named signal** — `cancel-requested`,
`reauthorize-requested`, `task-resolution-requested`, `unpark-requested` (reserved: no arm consumes it until
`DECISIONS.md` Q-13 gives it an origin route, D-122), each carrying only the
reference tuple and a `requestRef` or `taskRef` — to a running invocation's `listen` arm through
`…/invocations/{invocation_id}:plugin-control`, with a stated verb and payload shape, and **MUST**
hold a delivered signal until an arm consumes it (or reject it synchronously so the originating
control operation can answer `still-processing`). The platform has generic
`cancel`/`suspend`/`resume`/`retry`/`replay` control actions
([DESIGN.md](../../../serverless-runtime/docs/DESIGN.md) lines 883–889) and a plugin-control
passthrough whose verb set the plugin owns (line 893), but records "no signal delivery model"
([NEXT_ADR_SCOPE.md](../../../serverless-runtime/docs/NEXT_ADR_SCOPE.md) line 40, BR-108). In
addition, for the operator re-drive of D-86 and D-105: `:control` `retry` of a `failed` invocation (line 888)
**MUST** keep the `invocation_id`, so the re-drive resumes the instance Orders has bound rather
than starting a second invocation the binding refuses; it **MUST** resume execution **at the
faulted task** with the invocation's history, rather than re-running the document from its first
task — the platform states only "retry a failed invocation with same parameters" (line 888), and
the canonical definition is written for a resume: a restart from the top would re-enter the
approval stage with every round at `0` (`design/10-process-definition.md` §3.6 (a) `input.from`),
which the registry answers from its retained records, but a `listen` whose event was consumed
before the fault would wait for an event that is not delivered again, and every fixed wait would
restart (`design/01-foundation.md` §4.16); and `retry` **MUST** also be valid from
`dead_lettered`, keeping the `invocation_id`. The second clause is needed because `retry` is
listed as valid only from `failed` (line 888), while a failed invocation with no compensation
handler — this definition declares none (`design/10-process-definition.md` §3.1) — moves
`failed → dead_lettered` (line 458), so the state an operator finds is the one `retry` does not
accept. The platform also states two different paths for the same verb, and the ask is that it
names one: `:control` actions are executed by the host directly and "never reach the plugin"
(line 873), while the plugin-control passthrough routes "cancel / suspend / resume / retry" to the
plugin, which owns the verb set (line 893).

- **What the design cannot do until it lands**: hold and resume still arrive as Lifecycle events,
  but an operator cancel, a payment re-authorisation and a manual-task resolution — including the
  lifetime-ceiling task's retry, the one unpark route — cannot reach the running definition; the control operations record the
  request and answer `still-processing` indefinitely. The generic `:control` `cancel` is never a
  substitute, because it ends the invocation without the cancellation fence
  (`design/10-process-definition.md` §4.4). Until a `retry` that keeps the invocation and resumes
  at the faulted task is confirmed from both `failed` and `dead_lettered`, the `invocation-dead`
  task offers no re-drive, and the only way out for a dead invocation's order is the fallback of
  D-105: a Seller Operator's cancel, carried out in-process by the sweep, and a new order to
  re-acquire the customer (§4 item 11).
- **Who depends on the re-drive clause** (decision D-192): every invocation fault reaches the
  operator through the `invocation-dead` task, so every fault waits on this clause. That covers a
  step whose retries run out and a suspension that outlives the platform's limit. It also covers the two faults the design routes to it on purpose. One is an aged-out key of an
  operation that submits downstream, whose re-drive mints the successor `attempt`
  (`design/01-foundation.md` §4.3 *Aged-out key*, D-185). The other is a deterministic canonical
  `Internal` (500), which no `catch` matches (`design/01-foundation.md` §3.7 *A settlement that cannot commit
  aborts whole*, D-168). Each of these ends in the unwind and a lost in-flight order until the clause is
  confirmed.
- **Readiness**: the clause is an explicit item of the readiness gate of
  `design/01-foundation.md` §3.8. It is the one `p1` clause of this section that gates an operator
  action, not readiness: the gear reports ready without it, and the `invocation-dead` `retry` stays
  `action-not-offered` until the platform confirms it. The ask keeps `p1` (critical) because the
  PRD's recover-and-continue rule (§4 item 11) is met only once it lands.
- **Source**: `design/10-process-definition.md` §3.2 *Signal delivery*, §3.3, §4.4; `design/09-read-and-authz.md` §3.3, §3.6; `design/07-manual-tasks.md` §3.3, §4.4; `design/08-hold-and-cancel.md` §3.3; `design/01-foundation.md` §3.8, §4.16.

#### Retention of an event delivered between listens

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-upreq-serverless-runtime-event-retention-between-listens`

The plugin **MUST** retain a broker event that matches a running invocation's correlation — the
event type and correlation keys of a `listen` the definition declares — when it arrives while no
matching `listen` is armed, and **MUST** deliver it, in arrival order, to the next `listen` that
matches it, with no loss and no duplicate delivery. An invocation spends most of its time with no
`listen` armed for a given type: during every step call a stage makes, and between a competing
`fork`'s teardown and its re-arm, which the canonical definition passes through on every tick of
every loop (`design/10-process-definition.md` §3.6). The DSL states how a `listen` consumes events
while armed (dsl-reference.md *Listen*, *Event Consumption Strategy*) and nothing about an event
that arrives between two `listen` tasks; the platform's event-broker integration is "TBD per
deployment" (see *Event triggers over event-broker GTS events* above). The signals ask requires the
same retention for `:plugin-control` signals; this ask extends it to broker events.

- **What the design cannot do until it lands**: guarantee that an `OrderHeld`, `OrderResumed`,
  approval decision, `OrderAmended`, terminal order event or Subscriptions outcome is consumed.
  Only a lost resume has a stopgap: the eligibility and barrier polls evaluate only their own
  conditions, and nothing re-reads a hold, a decision or an amendment. A lost resume is applied
  from the Lifecycle order read by `apply-resume` `trigger: poll`, from the approval stage's
  resume wait (D-130) and from every other wait that holds a recorded suspension (D-133), at most
  about one `PT15M` poll late (one `PT1H` tick in the expected-time wait). A decision delivered during a probe is not recorded, so the gate escalates on
  its window; a Subscriptions outcome is recovered by the sweep,
  which reads it whether or not the event arrives (`design/05-provisioning-intents.md` §2.1).
- **Source**: `design/10-process-definition.md` §4.4 *Events delivered between listens*, §4.5
  (Q-11 (iv)); `design/08-hold-and-cancel.md` §4.7 items 3 and 10; `DECISIONS.md` D-124, D-130, D-133,
  Q-11.

#### A bound on one invocation's engine history

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-upreq-serverless-runtime-history-growth`

The plugin **MUST** bound the engine history of one `order_process` invocation over a life of 90
days and more — up to the declared `timeout_seconds` of 270 days (decision D-157) — while the definition loops on
fixed ticks, either by truncating that history inside its DSL interpreter (carrying the
invocation id, the workflow's `$context`, its task position and its pending signals and retained
events across the cut) or by stating a per-invocation history budget in events and bytes, and
**MUST** state how the tenant quota `max_execution_history_mb`
([DESIGN_GTS_SCHEMAS.md](../../../serverless-runtime/docs/DESIGN_GTS_SCHEMAS.md) line 1840)
applies: to one invocation or to the tenant's total. Every tick of the canonical definition is a
`wait`, a competing `fork` of up to seven branches torn down and re-armed, and one or two activity
calls; per waiting instance and day, the `PT30S` gate and barrier ticks add 2,880 iterations
each, a `PT5M` tick 288 and the overdue monitor 24 for the whole life
(`design/10-process-definition.md` §3.6 *History growth*). Serverless Workflow DSL 1.0.0 has no
construct that truncates history, so the definition cannot do it, and its ticks are fixed by the
± 5 min escalation accuracy (D-123) and the 15-minute fulfillment SLA.

- **What the design cannot do until it lands**: guarantee that an order waiting days for an
  approval, a manual task or its expected-fulfillment instant keeps its invocation. An invocation
  whose history exceeds an engine limit is ended by the engine; the instance liveness pass raises
  it as `invocation-dead`, and its re-drive, which resumes the same history, would reach the limit
  again, so the remedy left is the order cancel (D-105). The platform path does not report ready
  until this is answered.
- **Source**: `design/10-process-definition.md` §3.1, §3.6; `DECISIONS.md` D-127, D-128.

#### Start and generic control of the order process restricted to its owners

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-upreq-serverless-runtime-invocation-control-restriction`

The platform **MUST** let the owner of a Workflow restrict who may act on its invocations:
(1) **start** — `POST /api/serverless-runtime/v1/invocations` on `order_process` refused to every
caller, so that its two event-trigger bindings are its only start
([DESIGN.md](../../../serverless-runtime/docs/DESIGN.md) lines 865, 980–987);
(2) **generic control** — `…:control` `cancel`, `suspend` and `resume` refused on `order_process`
invocations to every caller, and `retry` admitted only for this gear's service principal, which
issues it from the `invocation-dead` task (lines 883–889); and (3) **bindings** — create, update,
enable and disable of an event trigger whose target is `order_process` restricted to the
platform-operator publish role that publishes the definition (lines 976–987). The platform
authorizes its own API (line 847) and states that the host "verifies tenant ownership of the
invocation" on plugin control (line 893). It states no per-function restriction of these actions,
and the event-trigger bindings are tenant-scoped platform objects (line 1146).

- **What the design cannot do until it lands**: detect rather than prevent. A generic `cancel`
  surfaces as `canceled`, which the instance liveness pass raises as an `invocation-dead` task
  (`design/01-foundation.md` §3.8). A generic `suspend` is indistinguishable from the `suspended`
  of a normal wait: it freezes the order, lifetime ceiling included, until the suspension times
  out into `failed` (line 455), and only then is it raised. A direct start or a hand-edited
  binding meets `admit-trigger`'s tenant, category and state checks and the one-active-instance
  index (`design/02-triggers-and-start.md` §2.2, §3.6), and binding drift is reported by the
  readiness check (`design/10-process-definition.md` §3.8, `DECISIONS.md` D-107).
- **Source**: `design/10-process-definition.md` §3.3, §3.8, §4.4; `design/01-foundation.md` §3.8;
  `design/02-triggers-and-start.md` §2.2; `DESIGN.md` §4.2 *Threat model*; `DECISIONS.md` D-105,
  D-107.

#### Operator visibility and re-drive of trigger-path dead letters

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-upreq-serverless-runtime-dead-letter-operator-visibility`

The event-trigger path's dead letters for this gear's triggers — a Lifecycle trigger or outcome
event the platform could not deliver to an invocation within its cap — **MUST** be visible to this
gear's operators with a stable identity, the order reference and the failure class, and **MUST**
support a re-drive that preserves the event identity. Dead-letter handling is named as a semantic
of event-driven invocation ([DESIGN.md](../../../serverless-runtime/docs/DESIGN.md) lines 808,
976) without an operator surface.

- **What the design cannot do until it lands**: `owf_dead_letter_triage` is not created and the
  three dead-letter routes of `design/09-read-and-authz.md` §4.1 are not served; a dead-lettered
  start trigger leaves an order in `submitted` with no instance and no manual task, visible only in
  the platform's trigger metrics. If the ask is declined, the table and the routes are retired
  rather than rebuilt on an Orders copy of the platform's record (`design/07-manual-tasks.md`
  §3.7).
- **Source**: `ADR/0009` as amended; `design/01-foundation.md` §4.8; `design/07-manual-tasks.md` §3.7; `DESIGN.md` §4.4.

#### Function-level failure handler as a compensation safety net

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-workflow-upreq-serverless-runtime-failure-handler-target`

The platform's function-level `on_failure` / `on_cancel` handler already takes any registered GTS
Function reference (`x-gts-ref: gts.cf.core.sless.function.v1~*`,
[DESIGN_GTS_SCHEMAS.md](../../../serverless-runtime/docs/DESIGN_GTS_SCHEMAS.md) lines 477–490)
and is invoked with a `CompensationContext` carrying the original invocation identity
([DESIGN.md](../../../serverless-runtime/docs/DESIGN.md) lines 402–413). A thin Function this gear
registers, whose only work is to call this gear's unwind step routes for the failed invocation's
instance, is therefore expressible today; nothing about targeting is asked. What remains asked is
**identity**: the outbound HTTP call that Function makes **SHOULD** be made as the
serverless-runtime service principal under the same terms as
`…-upreq-serverless-runtime-pdp-guarded-call`, carrying the failed invocation's
`resource_tenant_id` and its `invocation_id` from the `CompensationContext` — the value the
envelope binds every step call to (`design/01-foundation.md` §3.3 step 3, D-106) — so the step
route's principal check, the PDP's `process_step × execute` decision and the invocation binding
admit it. The canonical definition declares no handler today
(`design/10-process-definition.md` §3.1), because compensation is a path through Orders' own
operations; declaring one also changes the platform path of a failure from `failed →
dead_lettered` to `failed → compensating` (DESIGN.md line 457), which the re-drive of D-86 would
then have to accept.

- **What the design cannot do until it lands**: nothing it needs for correctness — the safety net
  for an invocation that ends without `report-outcome` is the reconciliation sweep's instance
  liveness pass and the `invocation-dead` task it raises, whose fallback is the sweep-driven
  dead-instance unwind (`design/01-foundation.md` §3.8, §4.16, D-105; `ADR/0005` as amended); the
  ask would only shorten the time to unwind.
- **Source**: `ADR/0005` as amended; `design/06-saga-and-compensation.md` §1.3.

## 3. Priorities

| Priority | Requirements |
|----------|-------------|
| `p1` (critical) | `…-upreq-overlap-presence-read`, `…-upreq-compensation-cancel-reason`, `…-upreq-explicit-start-instant`, `…-upreq-in-flight-rejection`, `…-upreq-cancel-accepted-transition`, `…-upreq-nonterminal-status-read`, `…-upreq-provisioning-latency-budget`, `…-upreq-identity-envelope-echo`, `…-upreq-payment-authorization-outcome`, `…-upreq-generic-approval-expectations-contract`, `…-upreq-submitted-ttl-visibility`, `…-upreq-lifecycle-thin-events`, `…-upreq-lifecycle-workflow-key-retention`, `…-upreq-catalog-dependency-topology-read`, `…-upreq-pii-classification-ruling`, `…-upreq-event-broker-shared-prerequisites`, `…-upreq-pdp-policy-integration` |
| `p1` (critical), platform path | `…-upreq-serverless-runtime-readiness-gate`, `…-upreq-serverless-runtime-event-triggers-gts`, `…-upreq-serverless-runtime-consumed-event-member-storage`, `…-upreq-serverless-runtime-pdp-guarded-call`, `…-upreq-serverless-runtime-attempt-and-deadline-propagation`, `…-upreq-serverless-runtime-history-residency-retention`, `…-upreq-serverless-runtime-definition-versioning-validation-hook`, `…-upreq-serverless-runtime-trigger-version-selection`, `…-upreq-serverless-runtime-signals`, `…-upreq-serverless-runtime-event-retention-between-listens`, `…-upreq-serverless-runtime-history-growth`, `…-upreq-serverless-runtime-invocation-control-restriction`, `…-upreq-serverless-runtime-dead-letter-operator-visibility` |
| `p2` (important) | `…-upreq-correlation-propagation`, `…-upreq-serverless-runtime-failure-handler-target`, `…-upreq-lifecycle-failure-reason-coverage` |

`cpt-cf-bss-orders-workflow-upreq-pdp-policy-integration` is `p1` because every caller-driven
operation fails closed until the catalogue, roles and the `assigned_principal` approver grant are
provisioned and verified (`ADR/0010`, `DECISIONS.md` D-63).

`cpt-cf-bss-orders-workflow-upreq-lifecycle-failure-reason-coverage` is `p2` because an interim
mapping exists and the exact reason survives on the Orders side (`design/06-saga-and-compensation.md`
§4.8); only Lifecycle's copy of it is approximate.

The serverless-runtime asks of §2.9 are `p1` for the **platform path** only: each gates whether
the canonical definition can run, not whether the process record and the step operations are
correct, which the fallback property keeps independent of them (`DESIGN.md` §4.9). The
failure-handler target is `p2` because the sweep's instance liveness pass already covers the case
it would shorten (D-105).

## 4. Required PRD Amendments

1. **`SUB-O` renumbering.** PRD §13 Dependencies (lines 1161-1172) and every in-body citation of
   `SUB-O6`-`SUB-O9` **MUST** be updated to cite `SUB-O11`-`SUB-O14` respectively, per the
   translation table in §2.1 ("Translation table") above, and the text **MUST** state that the old `SUB-O6` collided
   with the canonical `SEAMS.md` meaning. The PRD is the current definition site for the colliding
   `SUB-O6` and the unregistered `SUB-O7`-`SUB-O9`, so it is the artifact that must carry the fix.

2. **`OrderAmended` approval-verdict re-obtaining gap.** PRD §6.2 *Approval Request and Multi-Party Gate* (`PRD.md:277`) requires Workflow to
   cancel open approval gates for the prior version on `OrderAmended` and open new
   `OrderApprovalRequest`(s) for the new version when approval is required, but no PRD section
   requires **re-obtaining the approval-requirement verdict** for the new order version before
   doing so — it is implied, not stated. The PRD **MUST** add one or two sentences to §6.2 making
   explicit that on `OrderAmended`, Workflow **MUST** re-query the approval-requirement verdict
   keyed on the new `orderId` + `orderVersion` (the same verdict-acquisition path used for
   `OrderSubmitted`) and reflect it onward from `submitted`, rather than assuming the prior
   version's verdict or the presence of open gates alone determines the new version's requirement.
   Note: the sibling Lifecycle register (`gears/bss/orders-lifecycle/docs/UPSTREAM_REQS.md` §2.6,
   `upreq-workflow-amendment-verdict`) raises this as an ask on this gear and frames it more
   strongly — as a `MUST` obligation this gear has not yet met — than the PRD text actually
   supports; the PRD text only implies the re-obtain step through its gate-cancel/reopen language.
   This amendment closes that gap without a rewrite.

3. **Separation of duties on the approval gate.** PRD §9.2's expectations contract lists six
   clauses, none about separation of duties, and PRD §4 allows the Approver to be a seller
   operator — so an actor may submit an order and approve their own gate with every stated control
   passing. The PRD **MUST** add a clause **(g)** to the §9.2 expectations contract requiring the
   approval service to refuse a decision from the identity that submitted the order. This gear
   implements the local half today (`DECISIONS.md` D-56: the submitting identity is refused at the
   decision endpoint); the routing half belongs to the service and therefore to the contract the
   PRD owns.

4. **Privacy and PII classification.** See §2.6 above. The PRD's "Privacy / PII: not applicable"
   exclusion **MUST** be replaced with a positive data-classification ruling and a lawful-basis
   statement for the ≥ 400-day audit retention. Recorded as an ask rather than as a design change,
   since the PRD is approved input.

5. **§6.1 — platform execution with a gear-owned record.** `cpt-cf-bss-orders-workflow-fr-owf-process-state-nonauth`
   (PRD §6.1, lines 241–243) lists retry counters and durable timer handles among the state this
   gear must own. Under `ADR/0011` retry scheduling and timers are the platform plugin's; what this
   gear owns is the record of every attempt (`owf_step_log.attempt_id`), the remaining escalation
   window and the definition binding. The PRD **MUST** add one sentence to §6.1: *process
   execution MAY be driven by a platform workflow engine executing a versioned definition, provided
   every step is recorded in this gear's own process record in the step's own transaction, the
   definition version is pinned per instance, and engine history is never read as process state or
   audit*; and it **MUST** read "retry counters" and "durable timer handles" as "the attempt and
   timer evidence this gear records", not as state this gear schedules.

6. **§13 — durable-execution dependency row.** The *Durable execution infrastructure* row (line
   1168) says "selection pending ADR (see §15)". The PRD **MUST** replace it with a row naming
   **serverless-runtime** (`gears/serverless-runtime`, Temporal plugin) as the `p1` platform
   dependency selected by `ADR/0011`, keeping the sentence that engine history is not the
   process-audit SoR, and naming the readiness gate and the fallback property.

7. **§15 — Q-01 answer, and a new question on tenant-authored steps.** The Q-01 row (line 1183)
   **MUST** record the answer in two parts: the substrate choice is made (serverless-runtime,
   `ADR/0011`, 2026-09-24); the evaluation of engine-history isolation, retention and residency is
   pending on `…-upreq-serverless-runtime-history-residency-retention` (`DECISIONS.md` Q-12
   carries the pending half). The first criterion, *which commercial data would sit in engine
   history*, **MUST** be recorded with its qualifier, never as a bare "none". Commercial content in
   task data: none (`ADR/0013`). In history: identifiers (including `resource_tenant_id`), counters
   and array cardinalities, and the consumed events as published until
   `…-upreq-serverless-runtime-consumed-event-member-storage` or `…-upreq-lifecycle-thin-events`
   lands (`ADR/0013` as amended by D-131; D-66 as amended). A new §15
   row **MUST** register whether a seller-scoped publish role or tenant-authored Functions may
   adjust the definition in phase 1 (`DECISIONS.md` Q-10; this design recommends no).

8. **§9.1 — *Start workflow* realised by the platform trigger.** The *Start workflow* operation
   (line 691) **MUST** state that it is realised by the platform event triggers on
   `OrderSubmitted` and `OrderAmended`, whose invocation calls `admit-trigger` then
   `start-instance`, rather than by a REST route of this gear; its idempotency is the trigger
   family `{tenant}:{eventId}:admit-trigger` plus the one-active-instance index, which still
   satisfies "order ID + version" (`DECISIONS.md` D-73, D-74).

9. **§6.3 — payment authorization precondition.** The *Payment Authorization Precondition*
   (line 359) says begin-fulfillment "**MUST NOT** be called" on a failure without tolerate-failure;
   with Lifecycle as the sole evaluator of the seller's tolerate-failure policy, the PRD **MUST**
   say "begin-fulfillment is not **committed**" — a conclusive `failed` is passed to
   `begin-fulfillment` and a Lifecycle refusal is recorded as `withheld` (`DECISIONS.md` D-89).

10. **§9.2 — Generic Approval decision event carries references only.** The §9.2 expectations
    contract **MUST** add the clause of §2.3 above: the decision event carries references and the
    decision is read by `decisionEventId`, so no approver identity or reason crosses into engine
    history (`ADR/0013`).

11. **§6.1 and §7.1 — the dead-invocation fallback loses the in-flight order.**
    `cpt-cf-bss-orders-workflow-fr-owf-process-state-nonauth` (PRD §6.1, line 243) requires
    execution state to be "recoverable/replayable from that gear-owned record", and
    `cpt-cf-bss-orders-workflow-nfr-owf-durability` (PRD §7.1, line 562) requires an in-flight order
    to "be recoverable and continue execution". Under `ADR/0011`, continuation after a platform
    fault is the platform's re-drive of the same invocation (`DECISIONS.md` D-86, D-105). Until the
    platform confirms a re-drive that keeps the invocation and resumes at the faulted task
    (`…-upreq-serverless-runtime-signals`), an order whose invocation has died can only be
    unwound. So the PRD **MUST** add to §7.1:

    - *an in-flight order whose platform execution has ended and cannot be re-driven is raised as
      an order-scope manual task, and is either re-driven by the platform or, by a Seller
      Operator's decision, cancelled with compensation through the gear's own record*;
    - *re-acquisition is then a new order* — a `cancelled` or `fulfillment_failed` order cannot be
      amended (Orders Lifecycle `01 §3.7` transitions 13–27);
    - *this is a counted exception to "zero lost in-flight workflows", reported and retired once
      the platform re-drive is confirmed*.

    §6.1 **MUST** read "recoverable/replayable" as recoverable **from the record** — which the
    record still guarantees, because every committed step, the saga log and the manual-task
    history survive — and not as a promise that execution continues without the platform
    (`design/01-foundation.md` §4.16).

12. **§6.3, §7.1, §9.1 and §12 — dead letters are the platform's.** Under `ADR/0009` as amended
    and `DECISIONS.md` D-72, an inbound delivery past its cap is the platform event-trigger path's
    dead letter, and Orders writes no dead-letter row. Seven PRD clauses still require an Orders
    record: `cpt-cf-bss-orders-workflow-fr-owf-dead-letter` (`PRD.md:381`, a record carrying
    `orderId`, `orderVersion`, `correlationId`, source id and last error, with an alert to the
    fulfillment-operator queue); `cpt-cf-bss-orders-workflow-fr-owf-task-queue` (`PRD.md:421`,
    dead letters in the task queue); *Query process progress* (`PRD.md:692`, dead-letter records
    in the progress read); `cpt-cf-bss-orders-workflow-nfr-owf-audit` (`PRD.md:622`, "dead-letter"
    among the transitions this gear's audit log records); the §12 acceptance criteria 7c (`PRD.md:995`,
    "an inspectable dead-letter record **MUST** exist"), 7d (`PRD.md:1005`, an outcome still
    unknown after the sweep budget "**MUST** take the dead-letter path") and 8a (`PRD.md:1049`, a
    dead-letter record in the operator's scope appears). The PRD **MUST** restate them as follows:
    - the inspectable dead-letter record, its alert and its re-drive are the platform trigger
      path's, surfaced to this gear's operators through
      `…-upreq-serverless-runtime-dead-letter-operator-visibility`; until that ask lands, a
      dead-lettered trigger is visible only in the platform's trigger metrics, and a dead-lettered
      start trigger leaves the order in `submitted` with no instance and no manual task (§2.9);
    - the task queue and the progress read show platform dead letters once that ask lands, and
      are otherwise silent on them (`design/09-read-and-authz.md` §3.1, §4.1);
    - `nfr-owf-audit` covers the transitions this gear makes; a delivery the platform
      dead-letters never reaches a step operation, so no Orders audit row records it, and the
      platform's record is the evidence;
    - 7d: an intent whose outcome is still unknown after the sweep budget becomes `unresolved` and
      raises a line manual task through the definition's failure stage, never a dead letter
      (`design/05-provisioning-intents.md` §3.7, `DECISIONS.md` D-97, D-125).

13. **§1.4, §6.2, §6.3, §12, §14, §16 and §17.1 — the engine is selected and its timers are the
    plugin's.** Items 5 and 6 amend §6.1 and the §13 dependency row only. Seven more passages keep
    the pre-ADR-0011 wording and **MUST** be amended the same way: §6.2 *Escalation Timer*
    (`PRD.md:297`) makes Workflow "the **single owner** of scheduling, persisting, and firing
    escalation timers", and the §1.4 glossary entry *Escalation* (`PRD.md:107`) has escalation
    "triggered by a durable timer" — both **MUST** read that Workflow owns the escalation deadline
    on its record and the tick that re-checks it is a plugin timer of the definition
    (`DECISIONS.md` D-70, D-134); the §6.3 requirement
    `cpt-cf-bss-orders-workflow-fr-owf-provisioning-intent` (`PRD.md:349`, "Workflow **MUST** set
    a durable timer for that wait"), the §12 acceptance criterion (`PRD.md:910`, "keep a durable
    timer until expected fulfillment time") and the §17.1 diagram's "durable timer" nodes (`PRD.md:1232`, `PRD.md:1253`) **MUST** name the
    definition's fixed-granularity `wait` over a deadline Orders stores; §14 (`PRD.md:1177`, the
    engine "will be selected via ADR; this PRD … is agnostic to the engine choice") **MUST** record
    the selection (serverless-runtime, `ADR/0011`); and the §16 risk *Engine decision pending*
    (`PRD.md:1200`, "Design cannot begin") **MUST** be closed and replaced by the platform
    readiness-gate risk (`…-upreq-serverless-runtime-readiness-gate`).
14. **§6.2 — the escalation window is Orders' seller policy, not Generic Approval configuration.**
    §6.2 *Escalation Timer* (`PRD.md:297`) says "the Generic Approval service provides the
    escalation configuration". Under `DECISIONS.md` D-134 and D-140 the escalation window
    **values** (the default and per-party windows) are read from Orders' seller policy
    (`owf_seller_policy`, slice 01) and pinned on the gate row by `open-gates`. The sentence
    **MUST** read that Generic Approval provides the escalation **path** (the target the
    escalation command is routed to) and receives the command, while the escalation window is
    Orders' per-seller policy.

## 5. Traceability

- **PRD**: [`./PRD.md`](./PRD.md) — §6.1 (Fulfillment Plan Construction), §6.2 (Approval
  execution, `OrderAmended`), §6.3 (Provisioning Intents, retry/duplicate protocol, Intent
  Reconciliation Sweep), §6.4 (Cancellation fencing), §9.2 (External Integration Contracts), §13
  (Dependencies)
- **Design**: [`./DESIGN.md`](./DESIGN.md)
- **Upstream registers**: `gears/bss/subscriptions/docs/SEAMS.md` §I (`SUB-O1`..`SUB-O6`,
  canonical); `gears/bss/orders-lifecycle/docs/UPSTREAM_REQS.md` §2.1 (`SUB-O10` precedent, fork
  resolution stance) and §2.6 (`upreq-workflow-amendment-verdict`, the ask this gear inherits)
- **Registers with no receiving document**: Catalog and Payments have no seam map or upstream
  register in this repository, and the privacy ask has no named owner; §2.2, §2.5 and §2.6 are
  recorded here for whichever specification or owner eventually takes them.
- **Design and decision sources for the new asks**: `ADR/0007` (Lifecycle `submitted` TTL);
  `ADR/0008` and `DECISIONS.md` D-58 (platform producer outbox, §2.7 co-signature);
  `DECISIONS.md` D-16 (Catalog topology), D-46 and Q-02 (relational escalation threshold), D-50 and
  D-38 (audit retention and the privacy ask); `ADR/0010` and `DECISIONS.md` D-63 (platform PDP
  authorization, §2.8); `ADR/0011`, `ADR/0012`, `ADR/0013` and `DECISIONS.md` D-65…D-157, Q-10…Q-13
  (serverless-runtime, §2.9); `DECISIONS.md` D-110 (the `failure_reason` coverage ask, §2.4);
  D-91 and Q-08 (the overlap read's fail-closed reason, §2.1); D-97 as amended (no further
  Subscriptions ask, §2.1); D-72, D-65, D-105 as amended, D-134 and D-140 (PRD amendments 11–14, §4)
- **Platform register**: serverless-runtime has no upstream-requirements register; §2.9 cites its
  [DESIGN.md](../../../serverless-runtime/docs/DESIGN.md), [PRD.md](../../../serverless-runtime/docs/PRD.md),
  [NEXT_ADR_SCOPE.md](../../../serverless-runtime/docs/NEXT_ADR_SCOPE.md) and ADR-0004/0005 by line
