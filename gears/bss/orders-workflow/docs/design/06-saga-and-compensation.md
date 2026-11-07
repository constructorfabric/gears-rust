<!-- CONFLUENCE_TITLE: [BSS]: Orders Workflow — Saga and Compensation (Slice 6) -->
<!-- Related: ../DESIGN.md, ../PRD.md, ./01-foundation.md, ./05-provisioning-intents.md, ./10-process-definition.md, ./README.md | Owners: BSS Orders team -->

# DESIGN — Saga and Compensation (Slice 6)

<!-- toc -->

- [1. Architecture Overview](#1-architecture-overview)
  - [1.1 Architectural Vision](#11-architectural-vision)
  - [1.2 Architecture Drivers](#12-architecture-drivers)
  - [1.3 Architecture Layers](#13-architecture-layers)
- [2. Principles & Constraints](#2-principles--constraints)
  - [2.1 Design Principles](#21-design-principles)
  - [2.2 Constraints](#22-constraints)
- [3. Technical Architecture](#3-technical-architecture)
  - [3.1 Domain Model](#31-domain-model)
  - [3.2 Component Model](#32-component-model)
  - [3.3 API Contracts](#33-api-contracts)
  - [3.4 Internal Dependencies](#34-internal-dependencies)
  - [3.5 External Dependencies](#35-external-dependencies)
  - [3.6 Interactions & Sequences](#36-interactions--sequences)
  - [3.7 Database schemas & tables](#37-database-schemas--tables)
  - [3.8 Deployment Topology](#38-deployment-topology)
- [4. Additional context](#4-additional-context)
  - [4.1 Run-time guards the operations enforce](#41-run-time-guards-the-operations-enforce)
  - [4.2 The unilateral-cancel window and no Billing wait](#42-the-unilateral-cancel-window-and-no-billing-wait)
  - [4.3 Concurrent and late triggers (normative)](#43-concurrent-and-late-triggers-normative)
  - [4.4 Compensation cannot exceed the step's declared action](#44-compensation-cannot-exceed-the-steps-declared-action)
  - [4.5 The unilateral window is fence-visible](#45-the-unilateral-window-is-fence-visible)
  - [4.6 Upstream dependency answered: `SUB-O1`](#46-upstream-dependency-answered-sub-o1)
  - [4.7 Constraints this slice places on the definition](#47-constraints-this-slice-places-on-the-definition)
  - [4.8 The failure reason is resolved from the record and mapped (normative)](#48-the-failure-reason-is-resolved-from-the-record-and-mapped-normative)
  - [4.9 Every Lifecycle answer to `report-outcome` (normative)](#49-every-lifecycle-answer-to-report-outcome-normative)
- [5. Traceability](#5-traceability)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-design-saga-and-compensation`

## 1. Architecture Overview

### 1.1 Architectural Vision

This slice provides three **step operations** and sequences nothing itself.
`run-cancellation-fence` (protected) claims the one compensation run an order version may have,
stops dispatch, closes open approval records through slice 03's closure port and identifies every
intent already in flight. `compensate-order` (protected) is the **whole** reverse walk over the
persisted forward-execution ordinal: it reconciles the in-flight intents to terminal outcomes,
freezes the compensation subjects, and submits draft-void or activated-cancel compensating intents
one subject at a time, resuming where the previous pass stopped. `report-outcome` (protected) is
the only caller of the Lifecycle `fulfillment-acknowledgement` and `workflow-cancel` seam
endpoints, carrying compensation evidence it builds from this slice's records. The definition
fragment that orders them is
[`10 §3.6`](./10-process-definition.md#36-interactions--sequences) **(c)** — `compensateOrder`:
`run-cancellation-fence`, then `compensate-order` re-invoked until it answers `complete`, then
`report-outcome` and `terminate-instance` — entered from the partial-failure arm, the
pre-activation abort, the cancel path of **(d)** and the supersede and terminal-event paths of
**(f)**; fragment **(b)** calls `report-outcome` with `outcome: completed` after the last wave.
Orders' saga log — `owf_cancellation_fence` and `owf_compensation_record` — is the authority for
the walk; the definition only decides when it runs and what happens while it cannot finish
([`../ADR/0005`](../ADR/0005-cpt-cf-bss-orders-workflow-adr-saga-compensable-no-pivot.md) as
amended, [`../ADR/0011`](../ADR/0011-cpt-cf-bss-orders-workflow-adr-flow-as-platform-definition.md)).

The slice owns two requirements: classifying every fulfillment step as compensable or
irreversible at design time with a declared compensating action, and executing compensation on
order-level permanent failure, authorized cancellation, supersession and terminal order events.
It is the rollback half of the two-wave saga slice 05 (`05-provisioning-intents.md`) drives
forward: where slice 05 submits draft-create and activation intents, this slice submits their
compensating counterparts, draft-void and activated-cancel.

The stance is deliberately narrow. Both waves of this phase's fulfillment plan are compensable, so
there is no branch here for an irreversible step: OSS-side irreversibility is absorbed one layer
down, inside Subscriptions' own cancel handling, never by this gear reaching past Subscriptions.
Compensation is therefore always a Subscriptions-mediated provisioning intent under the identity
envelope and idempotency contract slice 05 owns, never a bespoke rollback channel.

Compensation is a reporting boundary, not a correctness gate on Billing. Operational compensation
completes or escalates on this gear's own timeline; the reversal of posted at-sale billable facts
lives in the billing chain and is never awaited before `report-outcome` reports to Orders
Lifecycle.

### 1.2 Architecture Drivers

#### Functional Drivers

| Requirement | Design Response |
|-------------|------------------|
| `cpt-cf-bss-orders-workflow-fr-owf-compensation-declaration` | §2.1 per-step compensability classification and the no-intra-saga-pivot principle; each operation's `compensation` field in §3.3 |
| `cpt-cf-bss-orders-workflow-fr-owf-compensation-execution` | §3.3 `run-cancellation-fence`, `compensate-order` and `report-outcome`; §3.6 the algorithms inside them; `10 §3.6` (c) the fragment that orders them; §4.7 the constraints the definition must satisfy |

#### NFR Allocation

| NFR ID | NFR Summary | Allocated To | Design Response | Verification Approach |
|--------|-------------|--------------|-----------------|----------------------|
| `cpt-cf-bss-orders-workflow-nfr-owf-idempotency` | Compensating actions must produce exactly one durable effect under retry | `compensate-order` (§3.3) | Every compensating intent carries a **five-component** idempotency key — `orderId` + `orderVersion` + `orderLineId` + `wave` + `intentKind`, prefixed with `resource_tenant_id`, with `wave_attempt` appended where the forward create was rebuilt and `comp_attempt` appended once above 1 — so the `draft_void` key is structurally distinct from the `draft_create` key it undoes and the `activated_cancel` key from the `activation` key (§2.1). A later pass of `compensate-order` or a platform retry of the same pass resubmits the **same** compensating key; an operator `retry` of the compensation task mints the next `comp_attempt`, so a leg Subscriptions settled `failed` is re-executed under a new key rather than answered with its stored failure (decision D-258, mirroring `wave_attempt`, D-119); the pass key (§3.3) only numbers the walk's resumptions | Replay test: re-running any pass against Subscriptions asserts a single terminal effect per subject. Negative test: the compensating key never collides with the forward key under `UNIQUE(idempotency_key)`, and Subscriptions never answers a compensating submission with the forward intent's stored outcome |
| `cpt-cf-bss-orders-workflow-nfr-owf-durability` | A compensating action must be executable independently of the forward step's completion state | `compensate-order` (§3.3) | Each subject's **current** phase — `draft`, `activated` or `absent` — is resolved through Subscriptions at compensation time before a leg is chosen; the walk's position lives in `owf_compensation_record`, not in the invocation, so a crash, a dead invocation or a new pass resumes at the first unsettled subject | Test: compensation issued after a forward step's local record is stale, missing, or after the draft's Subscriptions-side TTL has voided it still resolves to a defined leg and writes exactly one `CompensationRecord`; kill the platform worker mid-pass and assert the next pass resumes without a second submission |
| `cpt-cf-bss-orders-workflow-nfr-owf-fulfillment-sla` | Operational compensation must not block on financial reversal | `report-outcome` (§3.3) | `report-outcome` fires as soon as `compensate-order` answers `complete`, with no call into the billing chain on the reporting path | Test: outcome report latency is independent of billing-chain credit-note processing time |

#### Key ADRs

| ADR ID | Decision Summary |
|--------|-------------------|
| `cpt-cf-bss-orders-workflow-adr-saga-compensable-no-pivot` | Both waves of this phase are compensable; activation changes which compensating action applies (draft-void → activated-cancel) but never blocks rollback. As amended by ADR-0011: the structure of compensation is the definition's `try`/`catch`; the ordinal and the reverse walk stay in this gear as **one** operation |
| `cpt-cf-bss-orders-workflow-adr-flow-as-platform-definition` | The flow is a versioned platform definition; this slice provides operations and the saga record |
| `cpt-cf-bss-orders-workflow-adr-definition-versioning-and-protected-steps` | `run-cancellation-fence` **<** `compensate-order` **<** `report-outcome` is a publish condition, and each operation guards its own precondition at run time |
| `cpt-cf-bss-orders-workflow-adr-references-not-payloads` | Task inputs and outputs are references and small enums; compensation evidence, subscription identifiers and the cancel reason never cross the engine boundary |

### 1.3 Architecture Layers

```
serverless-runtime definition (10 §3.6 c)   — orders: fence, compensate (loop), report, terminate
        |  POST /bss-orders-workflow/v1/steps/{operation}  (references only)
        v
run-cancellation-fence   compensate-order   report-outcome       (this slice, inside the envelope)
        |                      |                    |
        |  closure port (03)   |  intent port (05)  |  fulfillment-acknowledgement / workflow-cancel
        v                      v                    v
owf_cancellation_fence   Subscriptions         Orders Lifecycle
owf_compensation_record  (draft-void | activated-cancel)
                               |
                               v
                     Policy Engine -> OSS  (never called by this gear)
```

| Layer | Responsibility | Technology |
|-------|---------------|------------|
| Sequencing | When the unwind runs, the re-invocation loop, escalation waits | Platform definition (`10 §3.6` (c), (d), (f)) executed by the serverless-runtime Temporal plugin |
| Application | The three step operations | Rust, inside the step envelope of [`01 §3.3`](./01-foundation.md#33-api-contracts) |
| Domain | Compensation record, fence state, compensation-failure reason | Domain entities (§3.1), no framework coupling |
| Infrastructure | Compensating-intent submission; Lifecycle outcome report | Slice 05's intent port and SDK client; the Lifecycle SDK client |

- [ ] `p3` - **ID**: `cpt-cf-bss-orders-workflow-tech-compensation-runtime`

The runtime of this slice is the Orders Workflow gear process hosting the step envelope; the
compensation walk executes inside `compensate-order`, never inside a platform worker, because the
walk's ordinal is this gear's record (ADR-0005 as amended). The platform's function-level
`on_failure` / `on_cancel` compensation layer
([serverless-runtime `DESIGN.md:402-409`](../../../../serverless-runtime/docs/DESIGN.md#compensation-design--two-layer-model))
is not used as the compensation mechanism. As a safety net a thin Function this gear registers can
already be the handler and call the unwind step routes, so targeting is not asked; what is asked is
the identity that call carries
([`…-upreq-serverless-runtime-failure-handler-target`](../UPSTREAM_REQS.md#function-level-failure-handler-as-a-compensation-safety-net),
ADR-0005 amendment).

## 2. Principles & Constraints

### 2.1 Design Principles

#### Every Step Is Classified at Design Time

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-workflow-principle-step-classification`

Every fulfillment step this gear defines **MUST** be classified, at design time and not at
runtime, as either compensable (with a declared compensating action) or irreversible. The
classification is recorded as each operation's `compensation` field in its slice's §3.3 and
mirrored into `owf_step_operation` (`01 §3.7`). This phase's fulfillment plan (slice 04,
`04-fulfillment-plan.md`) has exactly two waves, and both are compensable.

The classification covers **every registered fulfillment step**, not only the two waves:

| Step | Classification | Declared compensating action |
|------|----------------|------------------------------|
| `construct-and-freeze-plan` (slice 04) | compensable — no external durable effect | none required; the frozen plan is local state discarded with the process outcome |
| `evaluate-payment-auth-eligibility` (slice 04) | compensable — evaluation only, no external durable effect | **the empty action**: it authorizes no charge and creates no external object. Declared explicitly rather than left unstated |
| `begin-fulfillment` (the `approved -> in_fulfillment` Lifecycle transition, slice 04) | compensable | **`report-outcome`** (§3.3): acknowledging `in_fulfillment -> fulfillment_failed`, or submitting the workflow-mediated cancel, is what returns the order out of `in_fulfillment` |
| `dispatch-wave1-create` (slice 05) | compensable | **draft-void**, submitted inside `compensate-order` |
| `dispatch-wave2-activate` (slice 05) | compensable | **activated-cancel**, submitted inside `compensate-order` |

No step in this phase is classified irreversible. A compensable step's compensating action
**MUST** be idempotent (`cpt-cf-bss-orders-workflow-nfr-owf-idempotency`) and executable
independently of the forward step's completion state
(`cpt-cf-bss-orders-workflow-nfr-owf-durability`). The per-wave operations name
`compensate-order` as their `compensation` rather than a per-line undo operation, because the
undo of one line is only correct at its position in the walk.

**ADRs**: `cpt-cf-bss-orders-workflow-adr-saga-compensable-no-pivot`

#### No Intra-Saga Pivot

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-principle-no-intra-saga-pivot`

Activation **MUST NOT** be treated as a point past which rollback becomes impossible. What
activation changes is *which* compensating action applies — draft-void before activation,
activated-cancel after — never *whether* one exists. A compensating action that cannot be
completed **MUST NOT** be answered by inventing a further, third compensating action; it
**MUST** instead follow the escalation path required for permanent failure (manual task, order
remains non-terminal — `cpt-cf-bss-orders-workflow-fr-owf-manual-task`).

**ADRs**: `cpt-cf-bss-orders-workflow-adr-saga-compensable-no-pivot`

#### Compensating Keys Are Structurally Distinct From Forward Keys

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-principle-distinct-compensating-key`

A compensating intent's idempotency key **MUST NOT** equal the key of the forward intent it
undoes. The key composition is the **five-component** form slice 05 owns:

    {tenant}:{orderId}+{orderVersion}+{orderLineId}+{wave}+{intentKind}[+{wave_attempt}][+c{comp_attempt}]

where `{tenant}` is `resource_tenant_id`, `{wave}` is `owf_provisioning_intent.wave`
(`wave1_create | wave2_activate`) and `{intentKind}` is `owf_provisioning_intent.intent_kind`
(`draft_create | activation | draft_void | activated_cancel`) — the one template `01 §3.3`'s key
register, `05 §3.3` and the PRD state (decision D-289) — with the forward intent's
`wave_attempt` appended as the `[+{wave_attempt}]` tail where it is above 1 — a rebuilt create, or a line an
operator retried after its intent was recorded `failed` (decision D-119) — so the void of attempt 2
is distinct from the void of attempt 1. A compensating key also carries its own attempt,
`comp_attempt`, appended as `+c{comp_attempt}` once above 1 and minted **only** by an operator's
`retry` of the leg's compensation task (`07 §4.4`), exactly as a forward line's `retry` mints
`wave_attempt` (D-119): Subscriptions answers a seen key with its stored outcome, so a leg it settled
`failed` can be re-executed only under a new key (decision D-258). The tenant prefix namespaces every key.

Reusing the forward key is a correctness failure with three compounding effects: the local
`UNIQUE(idempotency_key)` constraint (`05-provisioning-intents.md` §3.7) rejects the compensating
row; Subscriptions, holding a settled record under that key, returns the **forward** intent's
stored outcome; and this slice would then write `outcome = succeeded` against a subscription that
is still live. A compensating submission that receives a settled outcome whose request fingerprint
does not match the compensating request **MUST** be treated as an `idempotency-key-conflict`
refusal, never as an absorbed duplicate.

This key is the **intent** key, and it is distinct from the step key of `compensate-order` itself
(§3.3), which numbers the walk's passes and never reaches Subscriptions.

**ADRs**: `cpt-cf-bss-orders-workflow-adr-idempotency-key-composition`,
`cpt-cf-bss-orders-workflow-adr-saga-compensable-no-pivot`

#### Compensation via Subscriptions Only, Never Direct to OSS

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-workflow-principle-compensation-via-subscriptions`

This gear never calls OSS directly to reverse a forward effect. Both compensating legs are
submitted as provisioning intents to Subscriptions, from inside `compensate-order`, through slice
05's intent port under the identity and idempotency contract slice 05 owns
(`cpt-cf-bss-orders-workflow-component-intent-dispatcher`). The definition calls no Subscriptions
endpoint (seam rules R1–R5, `10 §2`).

**ADRs**: `cpt-cf-bss-orders-workflow-adr-saga-compensable-no-pivot`

### 2.2 Constraints

#### Operational Compensation Never Waits on Billing

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-workflow-constraint-no-billing-wait`

Operational compensation **MUST NOT** block on, or wait for, a Billing credit note. Posted at-sale
billable facts are reversed in the billing chain, a distinct concern with a distinct owner (Orders
Lifecycle DESIGN §4.4, `gears/bss/orders/docs/features/06-workflow-seam.md`).
`report-outcome` reports the operational outcome as soon as `compensate-order` answers `complete`.

**ADRs**: none — a boundary constraint carried from the Lifecycle seam.

#### Distinct Manual-Task Reasons per Leg

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-workflow-constraint-distinct-compensation-reasons`

A draft that cannot be voided and an activated subscription that cannot be cancelled are distinct
failures with distinct remediation paths, and **MUST** produce distinct manual-task reasons
(§3.1 `CompensationFailureReason`). They **MUST NOT** be collapsed into one generic "compensation
failed" reason. Both are **enum** values on the producing side
(`owf_compensation_record.failure_reason`) and on the consuming side
(`owf_manual_task.failure_reason`, slice 07).

**ADRs**: `cpt-cf-bss-orders-workflow-adr-saga-compensable-no-pivot`

#### Fencing Precedes Any Compensated Outcome, On Every Unwind Path

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-workflow-constraint-fencing-precedes-outcome`

Before any compensated outcome is reported — `fulfillment_failed` on the failure path, `cancelled`
on the authorized-cancellation path, and the no-Lifecycle-call outcomes of the supersede and
terminal-event paths — the five fencing steps (§3.6) **MUST** have completed and **MUST** be
recorded on the `owf_cancellation_fence` row for that `(order_id, order_version)`. A late-arriving
success on an intent that was in flight when compensation began is a created subscription and
**MUST** be compensated.

The failure path is **not** exempt. Under the default remediate policy, independent in-flight lines
are allowed to finish (`PRD.md` §6.3), so an activation accepted before the failure determination
can confirm after it — the race the fence exists to close. The fence is a property of **reporting
a compensated outcome**, not of the cancellation trigger. The order is enforced twice: by the
definition's fence (`10 §4.1`, Unwind row) at publish, and at run time by `compensate-order` and
`report-outcome`, each of which refuses without a fence row (§4.1). A missing fence row is a
refusal, never a pass.

**ADRs**: `cpt-cf-bss-orders-workflow-adr-saga-compensable-no-pivot`,
`cpt-cf-bss-orders-workflow-adr-definition-versioning-and-protected-steps`

#### The Compensation Walk Is Sequential, Ordered, And Does Not Abort

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-workflow-constraint-sequential-ordered-walk`

Compensation legs **MUST** be executed **one subject at a time, in the frozen reverse-execution
order** (§3.6 *Compensation Walk Order*), inside `compensate-order`. Concurrent dispatch is
forbidden: reverse order is the whole correctness argument for the walk, and a concurrent walk has
no order. A definition **MUST NOT** fan the walk out per line (§4.7); the DSL has no dynamic
parallel construct anyway (`10 §2.2`), and a per-line definition task could only order by wave.

A leg that reaches `failed-pending-escalation` **MUST NOT** abort the walk. Aborting would leave
every not-yet-visited subscription live with no `CompensationRecord` row, which makes fence step 5
permanently unsatisfiable. The walk continues without exception: acquisition lines are independent
(decision D-196), so no subject is upstream of another and a leg that failed blocks no other
leg. (The former `blocked-upstream` outcome, which held a leg back while the plan's dependency
topology placed it upstream of a failed one, is withdrawn with the topology.)

**ADRs**: `cpt-cf-bss-orders-workflow-adr-saga-compensable-no-pivot`

#### One Compensation Run Per Order Version

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-workflow-constraint-single-compensation-run`

At most one compensation run may exist for an `(order_id, order_version)`. The
`owf_cancellation_fence` row is the mutual-exclusion token: its primary key is
`(order_id, order_version)`, so a second `run-cancellation-fence` — a second authorized
cancellation, or a cancellation arriving while a failure-path run is walking — **MUST** be absorbed
against the existing row rather than starting a second walk. The absorbing rules are §4.3 and are
normative.

**ADRs**: `cpt-cf-bss-orders-workflow-adr-saga-compensable-no-pivot`

## 3. Technical Architecture

### 3.1 Domain Model

**Technology**: Rust structs, same domain crate as slices 04-05.

**Location**: [`../DESIGN.md`](../DESIGN.md) §3.1 (gear-wide domain-model index); the entities below are normative in this slice, with their columns in §3.7.

**Core Entities**:

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-workflow-entity-compensation-record`

| Entity | Description | Schema |
|--------|-------------|--------|
| `CompensationRecord` | One row per compensation subject of a run: its frozen position in the reverse-execution walk, the leg (draft-void \| activated-cancel), the **target subscription reference**, the resolution mode (compensated \| already-absent), the outcome, and the evidence fields Lifecycle §4.4 requires (drafts voided, activations rolled back, at-sale facts emitted). Settles **in place** as passes of `compensate-order` advance it | `owf_compensation_record` table |
| `CompensationFailureReason` | Enumerated, leg-scoped reason for a compensating action that cannot complete: `draft-void-failed` (wave-1 leg) and `activated-cancel-failed` (wave-2 leg), kept distinct per `cpt-cf-bss-orders-workflow-constraint-distinct-compensation-reasons` | Enum column on `owf_compensation_record`, consumed as an enum by slice 07 |
| `CompensationSubject` | The set of subscriptions one run must reach, frozen once fence step 3 completes: every subscription this gear created for the order version (from `owf_provisioning_intent`, including late successes and slice 05's unmatched confirmations recorded against this version) **plus** every subscription attached by a verified manual override (slice 07), which has no provisioning-intent row. Materialized as the `CompensationRecord` rows of one run, in walk order | rows of `owf_compensation_record` for one `(order_id, order_version)` |
| `CancellationFenceState` | Per-order-version progress of the five fencing steps, the trigger that claimed the run (and any promotion), the references `report-outcome` needs (failure reason, cancel request), and the mutual-exclusion token for the run | `owf_cancellation_fence` table |

**Relationships**:
- `CompensationRecord` → `cpt-cf-bss-orders-workflow-entity-provisioning-intent` (slice 05): one record targets exactly one prior provisioning intent's resulting subscription, and names the compensating intent row submitted for it. The forward reference is nullable only for an override-attached subscription.
- `CompensationRecord` → `cpt-cf-bss-orders-workflow-entity-manual-task` (slice 07): an override-attached subscription's `subscription_ref` is the `subscriptionId` slice 07's `verify-override` verified and recorded; a `failed-pending-escalation` record names the manual task slice 07's creator returned.
- `CancellationFenceState` → `CompensationRecord`: one fence state aggregates the records of one `(order_id, order_version)`; step 5 is satisfied only when every record is `succeeded`.

### 3.2 Component Model

Each component is the owner of one step operation. None holds a timer, a retry loop or an event
subscription: when the unwind runs and how often `compensate-order` is re-invoked are the
definition's (`10 §3.6` (c)); a transient failure is re-issued under the same key by the
definition's retry policy (`10 §2.2`); Subscriptions confirmations reach this gear through Orders'
own consumer and slice 05's `reconcile-intent`, never through a subscription here.

```mermaid
graph LR
    DEF[Definition 10 §3.6 c/d/f<br/>serverless-runtime plugin] -->|run-cancellation-fence| CF[Cancellation-Fencing Component]
    DEF -->|compensate-order, pass n| CE[Compensation-Execution Component]
    DEF -->|report-outcome| OR[Outcome-Report Component]
    CF -->|closure port| AGM[Approval Gate Manager, slice 03]
    CF --> FENCE[(owf_cancellation_fence)]
    CE -->|reconcile port| RI[reconcile-intent, slice 05]
    CE -->|intent port: draft_void / activated_cancel| SUB[Subscriptions]
    CE -->|creation port, leg-specific reason| MTC[Manual-Task Creator, slice 07]
    CE --> REC[(owf_compensation_record)]
    CE --> FENCE
    OR -.gate: no_active_verified_at.-> FENCE
    OR -.evidence.-> REC
    OR -->|fulfillment-acknowledgement / workflow-cancel| LC[Orders Lifecycle]
    OR -->|OrderFulfillmentCompleted / Aborted| OUTBOX[(platform producer outbox)]
```

#### Compensation-Execution Component

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-workflow-component-compensation-executor`

##### Why this component exists

Order-level permanent failure, authorized cancellation, supersession and terminal order events all
require the same rollback — undo every subscription created for the order version, in reverse
order — so one component owns that behaviour as one operation, `compensate-order`.

##### Responsibility scope

Owns `compensate-order` (§3.3). Each pass: on a spawned order first sends the revoke of every
`owf_receiver_revoke` row the fence froze that has no attestation yet, while the pass's remaining
deadline exceeds one revoke and one walk leg (`inst-co-revoke`, D-269, D-299, D-313);
completes fence step 3 by reconciling the intents fence step 2 identified; once step 3 is complete, **freezes the walk** — enumerates the subjects and
assigns each a `compensation_sequence` from its forward acceptance ordinal
(`owf_provisioning_intent.execution_seq`, assigned by slice 05 at acceptance) or, for an
override-attached subscription, from the ordinal minted when the override was verified (D-268); then **executes the walk** one
subject at a time in descending `compensation_sequence`, resolving each subject's current phase
through Subscriptions and taking one of three branches:

| Resolved phase | Action | `resolution_mode` |
|----------------|--------|-------------------|
| `draft` | submit a `draft_void` provisioning intent through slice 05's intent port | `compensated` |
| `activated` | submit an `activated_cancel` provisioning intent through slice 05's intent port | `compensated` |
| absent — already voided, cancelled, or expired under Subscriptions' own draft TTL | submit nothing; record the leg that *would* have applied with `outcome = succeeded` | `already-absent` |

An `already-absent` resolution is a *verified* absence — a successful status read returning
not-found or a terminal voided / cancelled state — never an inference from a failed or timed-out
read, which leaves the subject unsettled for the next pass.

*Bounding a leg.* A compensating intent is an ordinary provisioning intent and keeps slice 05's
bounds: an accepted-but-unconfirmed intent is reconciled by `reconcile-intent` on the standard
ladder and terminates at the sweep floor of **30 reads or 23 h, whichever comes first**. A
submission that Subscriptions does not accept is counted on the record
(`submission_failure_count`), because the definition's retry policy counts calls of the whole
pass, not submissions of one leg; the leg's bound is the retry-budget baseline of `01 §4.2`
(5). Whichever bound is reached first writes `outcome = failed-pending-escalation` with the
leg-specific `CompensationFailureReason`. There is no third attempt path.

*Escalating.* On `failed-pending-escalation` the operation **invokes slice 07's single
manual-task creation port** (`cpt-cf-bss-orders-workflow-component-manual-task-creator`) in the
same unit of work, with the leg-specific reason, **under either partial-failure policy**. A
compensation failure is never an incident, because the order is not terminal while a subscription
survives (`07 §2.2` *Compensation Failure Is Always Actionable*). It does not write
`owf_manual_task` itself.

##### Responsibility boundaries

Does not decide *whether* to compensate — the definition enters the unwind from a determination
another operation recorded. Does not call OSS. Does not wait on or query the billing chain. Does
not invent a compensating action beyond the declared one. Does not create the manual-task row.
Does not report the order-level outcome. Does not hold a timer: between passes nothing in Orders
runs for the walk; the next pass is the definition's.

**Retired from this component** (ADR-0011): the bespoke waiting between legs and between
reconciliation reads, and the "engine retry budget and step deadline" bound on submissions, now
the definition's re-invocation loop (`10 §3.6` (c) `awaitCompensationResolution`) and the
per-leg counter above.

##### Related components (by ID)

- `cpt-cf-bss-orders-workflow-component-intent-dispatcher` (slice 05) — calls its intent port
  with `intentKind` set to the compensating kind; shares its identity envelope and key derivation.
- `cpt-cf-bss-orders-workflow-component-reconciliation-sweep` (slice 05) — calls its
  `reconcile-intent` unit in-process for in-flight forward and compensating intents; the sweep
  worker's instance liveness pass is the backstop for an invocation that died, and drives the
  dead-instance unwind after an operator's cancel (`01 §3.8`, `01 §4.16`, D-105).
- `cpt-cf-bss-orders-workflow-component-cancellation-fencer` — requires its fence row (depends on).
- `cpt-cf-bss-orders-workflow-component-outcome-reporter` — reads this component's records.
- `cpt-cf-bss-orders-workflow-component-manual-task-creator` (slice 07) — the sole call site
  for a `failed-pending-escalation` leg, under either policy (calls).
- `cpt-cf-bss-orders-workflow-component-override-verifier` (slice 07) — supplies the verified
  `subscriptionId` of an override-attached subscription (depends on).

#### Cancellation-Fencing Component

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-workflow-component-cancellation-fencer`

##### Why this component exists

An unwind can race with in-flight provisioning intents and with open approval records; without an
ordered, recorded fence, a subscription created after compensation "finished" would survive a
cancelled or failed order, and a gate could still be decided for an order being unwound.

##### Responsibility scope

Owns `run-cancellation-fence` (§3.3) — fencing steps 1 and 2 — on every unwind path (failure,
cancel, supersede, terminal event): claiming the `owf_cancellation_fence` row, which is the
one-run-per-order-version token; moving the phase projection to `compensating`, which every
dispatch operation of slice 05 refuses from; closing open approval records through slice 03's
closure port; identifying every accepted or in-flight intent; recording the references
`report-outcome` needs; and absorbing a second or late trigger against an existing run (§4.3).
Under `fail-fast` on the failure trigger it also hands each failed or unresolved forward line to
slice 07's Incident Recorder (`cpt-cf-bss-orders-workflow-component-incident-recorder`), which is
the fail-fast destination of a **forward** failure and never of a compensation leg — except a line
with an open task: that task is the line's object, never both (decision D-300). Under `remediate`
it does the same for a failed or unresolved line with no task when the cause is one the definition
unwinds under either policy with no task — an external activation or `order-binding-expired` — so
a line listed in the same answer is never left with neither (decision D-330). Owns the
unilateral-cancel-window rule for a `FulfillmentTask` (§4.5).

Steps 3–5 are recorded on this component's row but advanced by `compensate-order`, because each
needs the reconciliation and the walk that operation owns.

**On a spawned order the stop includes the receivers** (Lifecycle D-198; decisions D-243, D-248,
D-269). Lifecycle requires an ordinary cancel or failure to "revoke all receiver generations, tombstone
outstanding create keys"
([Lifecycle `DESIGN.md`](../../../orders/docs/DESIGN.md#staged-holdcancelfailure-without-new-public-states)
step 3). In step 1 the fence therefore **freezes the revokes and sends none**: in its unit of work
it inserts one `owf_receiver_revoke` row (§3.7) for each generation of the version recorded in
`owf_fulfillment_grant` (slice 05) whose `close_kind` is not `revoked` — the open grant, a
generation closed `paused` by a post-spawn hold (the receivers stay paused after an abandoned
control, D-244), and one closed for a replacement — and each receiver of that generation's roster,
under the revoke key `{tenant}:{orderId}:{orderVersion}:receiver-revoke:{generation}:{receiverRef}`,
and stamps each such generation's `revoke_frozen_at` (`05 §3.7`, D-291).
The fence thereby stays what its 5 s deadline and `submits_downstream = false` declare: a
record-only operation that calls nothing outside Orders' database (`01 §3.3`, §3.7).
`compensate-order` sends the revokes: on every pass, before anything else of the pass reaches a
downstream, it sends each unattested row its revoke under the row's unchanged key — the revoke
tombstones the generation's outstanding create keys, so no delayed create or activation of the
attempt can still apply — and writes the attestation the receiver returns on the row, once; an
unavailable or silent receiver leaves it absent, never inferred revoked (`inst-co-revoke`). It
sends a row only while the pass's remaining deadline still exceeds one revoke call and one walk
leg, so a receiver outage never spends the pass the walk needs; an unsent row waits for the next
pass (§3.3 *Per-pass deadline*, D-299). Step 5
then requires every row attested, records each such generation `revoked` on its grant — a row
closed `paused` or `closed` is rewritten `revoked` once, the one rewrite slice 05's table allows,
the earlier state kept in the write-once `revoked_from` (`paused`, `closed`, or `open` for an
unclosed row, D-313) —
and, before it answers `complete`, reads Lifecycle's current grant
(`…-upreq-lifecycle-current-grant-read`): a generation this gear has not recorded is recorded
through slice 05's grant port with the predecessor and the `source` the read names (until the read
names the source, derived from that predecessor as this gear recorded it — closed `paused`, or
`revoked` with `revoked_from = paused` → `resume`, otherwise → `replace` — and never refused, D-291,
D-296, D-313); an
unclosed predecessor is never closed `closed` without an attestation but stays pending revoke, outside the one-open-grant index once its revoke rows
are frozen (`revoke_frozen_at`), and is recorded `revoked` once they are attested; every
generation not `revoked` whose `revoke_frozen_at` is null — recorded
after the fence froze the rows, by this step or by another `record_grant` writer — gets its rows, and
the pass answers `in-progress`; the test is `revoke_frozen_at`, never whether rows exist, so an
unfrozen generation is never read as attested and a frozen one whose roster names no receiver is
vacuously revoked (D-288, D-298). Such a generation is a
successor a post-spawn resume or a D-201 replacement committed while the unwind ran:
Lifecycle learns of the unwind only from `report-outcome`, so until then it still admits a hold and
a resume, and the unwind's forks carry no resume arm (`10 §2.2` rule 8). `report-outcome` repeats the
read before its Lifecycle call and sends the run back for one more pass when it finds one
(`lifecycleCall = revoke-pending`). The barrier evidence — `receiver_barrier_digest` and the typed
contribution `report-outcome` sends beside the five-field compensation evidence (Lifecycle
`06-workflow-seam.md` §2.4 step 2) — is built from the rows. A version with no recorded grant — no
spawn signal — has no receiver generation, and the fence freezes nothing.

**A pending pause control** of slice 08 — one `staged` or `pausing` — is marked `superseded` by
the fence in the same unit of work, and a control the intake receives once the instance is
`compensating` is recorded `superseded` at once and signals nothing — the intake reads the phase
under the instance row lock the fence takes, so a control is either superseded by the fence or
recorded after it (`08 §3.3`, D-297). Lifecycle answers
competing ordinary control requests `still-processing` until the owning control settles, so
`report-outcome`, before its Lifecycle call, settles every `superseded` control not yet settled
there through Lifecycle's `supersede_fulfillment_control` (Lifecycle
`DESIGN.md#contract-06-supersede-fulfillment-control`, `bss/orders-stage4` D-259 *A pending pause
control is superseded by Workflow's revoke evidence*), with the revoke evidence of the control's
generation built from the rows (§3.6 `inst-ro-receivers`). Until Lifecycle implements it, the
unwind reports with the control still pending at Lifecycle, and Lifecycle's `still-processing`
answer to `report-outcome` is the documented residual.

##### Responsibility boundaries

Does not submit compensating intents. Does not decide *whether* the order failed or was
cancelled, and does not choose the Lifecycle report — the trigger records that, and the fence is
identical on every path. Does not wait: fence step 3's completion is observed by later passes of
`compensate-order`. The component name is retained for continuity with `PRD.md` §6.4's
"cancellation fencing" wording.

**Retired from this component** (ADR-0011): "waits for" reconciliation in step 3 and "drives"
the executor in step 4 — sequencing is the definition's.

##### Related components (by ID)

- `cpt-cf-bss-orders-workflow-component-compensation-executor` — precedes (the definition calls
  `compensate-order` only after this operation settles).
- `cpt-cf-bss-orders-workflow-component-approval-gate-manager` (slice 03) — calls its closure
  port `close_open_approvals(correlationId, reason)` in step 1.
- `cpt-cf-bss-orders-workflow-component-intent-dispatcher` (slice 05) — reads
  `owf_provisioning_intent` in step 2; its dispatch operations refuse once the phase is
  `compensating`.
- `cpt-cf-bss-orders-workflow-component-incident-recorder` (slice 07) — fail-fast forward
  failures, and under `remediate` a taskless line of an either-policy no-task unwind (D-330); never
  a compensation leg (calls).

#### Outcome-Report Component

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-workflow-component-outcome-reporter`

##### Why this component exists

Reporting to Lifecycle and enqueueing `OrderFulfillmentCompleted` / `OrderFulfillmentAborted` is
a distinct concern from executing compensation, and must fire only after operational
compensation reaches a known outcome — never gated on financial reversal.

##### Responsibility scope

Owns `report-outcome` (§3.3), the **only** caller in this gear of Lifecycle's
`OrdersWorkflowSeamV1::acknowledge` and `OrdersWorkflowSeamV1::workflow_cancel`
(Lifecycle D-155; decision D-193), the SDK form of `POST …/orders/{orderId}/fulfillment-acknowledgement`
and `POST …/orders/{orderId}/workflow-cancel`. Slice 08 no longer calls
`workflow-cancel`; its `authorize-cancel` records the authorization this operation reads. The
report mode follows the fence row's (possibly promoted) trigger:

| Mode (`outcome`) | Precondition in Orders' record | Seam call | Event enqueued |
|------------------|-------------------------------|-----------|----------------|
| `completed` | no fence row; slice 04's completion predicate holds (every task `activated` — the Subscriptions transition read back as `applied`, never `approved` with OSS pending (Lifecycle D-165; decision D-195) — with a non-null `subscription_id`, the identifiers distinct across the plan's tasks) | `fulfillment-acknowledgement`, trigger `acknowledge-completed`, with the per-line subscription identifiers | `OrderFulfillmentCompleted` |
| `failed` | fence `no_active_verified_at` set, trigger `failure` | `fulfillment-acknowledgement`, trigger `acknowledge-failed`, with compensation evidence and the fence row's `failure_reason`, mapped from the recorded cause (§4.8) | `OrderFulfillmentAborted` with `mode = failed` |
| `cancelled` | fence `no_active_verified_at` set, trigger `cancel` | `workflow-cancel`, trigger `cancel-workflow-mediated`, with compensation evidence and the `cancel_reason` of the fence row's cancel request (`09 §3.7`) | `OrderFulfillmentAborted` with `mode = cancelled` |
| `superseded` | fence `no_active_verified_at` set, trigger `supersede` | **none** — the order is live at the new version | `OrderFulfillmentAborted` with `mode = superseded` and `retainedObligations[]` (D-234; supersession is admissible only before `in_fulfillment`, so `fulfillmentBegun = false`) |
| `terminal-event` | fence `no_active_verified_at` set, trigger `terminal-event` | **none** — the order is already terminal in Lifecycle | `OrderFulfillmentAborted` with `mode = terminal-event` and `retainedObligations[]`, whether or not `begin-fulfillment` had settled (D-234) — including the operator-forced unwind of D-208 and a failure or cancel before fulfillment that step 4 reports as `terminal-event` |

*Only from fulfillment* (decision D-109). `failed` and `cancelled` reach Lifecycle only for a
version whose `begin-fulfillment` committed (`owf_fulfillment_plan.begin_fulfillment_committed_at`,
slice 04): Lifecycle admits `acknowledge-failed` and `cancel-workflow-mediated` from
`in_fulfillment`, or from `on_hold` with pre-hold `in_fulfillment`, and from nothing else
([Lifecycle `01 §4.3`](../../../orders/docs/features/01-foundation.md#contract-01-4-3)
rows 14, 16, 26, 27), and its permission matrix gives this gear's principal no other cancel
([Lifecycle `08 §4.3`](../../../orders/docs/DESIGN.md#contract-08-4-3)).
A cancel of an order not yet in fulfillment is Lifecycle's ordinary cancel: the Workflow cancel
route refuses it and `authorize-cancel` does not authorize it while the order is live (`09 §3.3`,
`08 §3.6`); a plan-level failure passes `begin-fulfillment` before its unwind (`04 §4.3`). A run
that nevertheless reaches the report without a committed begin — a cancel authorized after the
order ended by another path — makes no Lifecycle call and records `terminal-event` (§3.6
`inst-ro-reauthorize`). The same holds where Lifecycle answers that the order is already terminal
(§4.9).

*Receivers first* (decision D-269). On a spawned order a `failed` or `cancelled` report first
re-reads Lifecycle's current grant and the version's `owf_receiver_revoke` rows: a generation this
gear has not recorded, a generation whose rows are not frozen (`revoke_frozen_at` null, D-298), or a
row not yet attested, answers `lifecycleCall = revoke-pending`, which
sends the definition back to `compensate-order` for one more pass; then every `superseded` pause
control not yet settled at Lifecycle is settled through `supersede_fulfillment_control` (§3.6
`inst-ro-receivers`).

*The reporting gate.* A compensated outcome is reportable only when the fence row has
`no_active_verified_at` set, which requires **every** `CompensationRecord` of the run to be
`succeeded`. `failed-pending-escalation` is a *known* outcome for a leg — the operation has stopped
attempting it — but it is **not** a *compensated* outcome: a run containing one
`failed-pending-escalation` record has at least one live subscription, and
reporting would assert to Lifecycle — and to Billing downstream — that none survived. The report
is refused (`outcome-not-reportable`, §3.3).

The refusal is bounded. The order stays non-terminal under the escalated manual task whose SLA is
the operator-facing clock (slice 07); remediation is declared exhausted after **3 failed operator
resolution attempts on the same task, or the manual-task SLA deadline elapsing without
resolution, whichever comes first** (`../DECISIONS.md` D-55). Above that sits the non-pausable
`max_process_lifetime` of **90 days** (D-53), now the definition's top-level lifetime `wait`
(`10 §3.6` (a)). What lifts the refusal is always the same: the blocked leg reaches `succeeded`,
by operator retry under the next `comp_attempt` (an `override` is not offered on a compensation
task, `07 §4.4`; decision D-258), and a later pass of `compensate-order` sets
`no_active_verified_at`.

*Publishing.* The event is **enqueued through the bound platform producer outbox
(`toolkit_db::outbox`) in the same local transaction that records the reported outcome**, after
the Lifecycle call has returned its acknowledgement and before the step's answer is sent, per
`cpt-cf-bss-orders-workflow-adr-outbox-process-events` (`01 §3.6`, §4.7). A crash between the
Lifecycle acknowledgement and that transaction replays the call under the same lifecycle-transition
key, which Lifecycle absorbs and answers with its stored outcome — or, once Lifecycle's ≥ 30-day
workflow-class receipt floor has passed (D-203), refuses `not-admissible` on an order the read shows the report applied to,
which answers `already-applied` (§4.9, decision D-188) — so the event is enqueued exactly once per
reported outcome.

##### Responsibility boundaries

Does not call the billing chain and does not delay reporting for it
(`cpt-cf-bss-orders-workflow-constraint-no-billing-wait`). Does not decide compensation success or
failure. Does not publish to the bus directly. Does not re-check cancel authority: the re-check
runs in `authorize-cancel` before the fence and in `compensate-order` before the first compensating
leg (slice 08); once every subscription has been removed, withholding the report would leave
Lifecycle asserting subscriptions that no longer exist (decision D-84 as amended: the apply-time
re-check runs at `pre-fence` and `pre-compensation`, and `report-outcome` does not repeat it).

##### Related components (by ID)

- `cpt-cf-bss-orders-workflow-component-compensation-executor` — reads its records.
- `cpt-cf-bss-orders-workflow-component-cancellation-fencer` — reads its row as the gate.
- `cpt-cf-bss-orders-*` (Orders Lifecycle gear, out of this gear's ID space) — reports
  outcome to, bound by reference to Lifecycle DESIGN §4.4 (depends on, cross-gear).

### 3.3 API Contracts

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-workflow-interface-compensation-trigger`

- **Contracts**: `cpt-cf-bss-orders-workflow-contract-step-invocation`, `cpt-cf-bss-orders-workflow-contract-owf-provisioning-intent`
- **Technology**: internal REST step surface `POST /bss-orders-workflow/v1/steps/{operation}` of [`01 §3.3`](./01-foundation.md#33-api-contracts); callable only by the serverless-runtime service principal under PDP resource `gts.cf.bss.orders_workflow.process_step.v1~` × action `execute` with `operation` as the resource property
- **Location**: this section; mirrored into `owf_step_operation` (`01 §3.7`)

The three operations below are registered against the operation registration boundary
([`01 §3.2`](./01-foundation.md#32-component-model)) with the contract fields of
[`01 §3.3` *The step-operation contract*](./01-foundation.md#the-step-operation-contract). Every
input carries the reference tuple of `10 §3.6` — `correlationId`, `orderId`, `orderVersion`,
`resourceTenantId`, `invocationId`, `attemptId` — written **ref** below; every other member is a
reference or a small enum. Every operation resolves `correlationId` to this slice's rows and reads
commercial order context — subscription identifiers, the roster, the cancel reason, evidence —
inside Orders, from its own record or from Lifecycle under R1, under the instance's
`resource_tenant_id` and `seller_tenant_id` (ADR-0013); none of it is in an input or an output.
Every operation answers `permanent-failure` with `version-mismatch` on a terminal instance
(`01 §3.3` `terminate-instance`).

| `name` | `protection` | `input` | `output` | `idempotency_key` | `declared_event` | `compensation` | `reasons` | `audit_kind` | `retry_class` | `deadline` |
|--------|--------------|---------|----------|-------------------|------------------|----------------|-----------|--------------|---------------|------------|
| `run-cancellation-fence` | `protected` | ref + `trigger` ∈ `failure` · `cancel` · `supersede` · `terminal-event`; `cancelRequestRef` (slice 08's request reference, on `cancel` only); `triggerEventId` (on `supersede` and `terminal-event`). No failure reason is an input: on `failure` the fence resolves it from the cause it found and maps it (§4.8) | `fenceRef`, `claim` ∈ `claimed` · `absorbed`, `effectiveTrigger` (the row's trigger after any promotion), `inFlightCount` | instance-scoped `{tenant}:{correlationId}:run-cancellation-fence:{trigger}[:{triggerRef}]`, where `triggerRef` is `cancelRequestRef` on `cancel` and `triggerEventId` on `supersede` and `terminal-event`, and is absent on `failure` (§4.3 *One key per trigger request*) | none | none — the fence is a path step, not an undoable effect | `version-mismatch`, `not-found` (the record holds no cause for this trigger, §3.6 `inst-fence-cause`), `outcome-already-reported` (a completion report, a rejection reflection or the run's own report of this instance has settled, `inst-fence-cause`, D-251, D-271), `idempotency-key-conflict`, `per-attempt-timeout` | `phase-transition` (`started`/`suspended`/`parked` → `compensating`) on a claim; `step-completion` on an absorption | `retryable-on: transient` | 5 s |
| `compensate-order` | `protected` | ref + `pass` (0 on first entry, else the previous answer's `nextPass`, or previous `pass` + 1 after an exhausted retry; validated by the operation's own pass guard against the fence row's `last_pass`, never by the envelope's round counter: any pass above the highest presented is admitted, `01 §3.3` *Rounds and attempts* rule 2, §3.6 `inst-co-fence-guard`, decision D-247) | `compensationState` ∈ `complete` · `in-progress` · `pending-escalation`; `nextPass`; `taskRefs[]` (manual tasks raised for `failed-pending-escalation` legs, this pass or earlier) | step: instance-scoped `{tenant}:{correlationId}:compensate-order:{pass}`; per leg: intent family `{tenant}:{orderId}+{orderVersion}+{orderLineId}+{wave}+{intentKind}[+{wave_attempt}][+c{comp_attempt}]` with `intentKind` ∈ `draft_void` · `activated_cancel` (§2.1; `comp_attempt` appended once above 1, minted only by the compensation task's `retry`, decision D-258) | none — `OrderFulfillmentAborted` belongs to `report-outcome` | none — compensation is not itself compensable (§2.1 *No Intra-Saga Pivot*) | `fence-not-claimed`, `draft-void-failed`, `activated-cancel-failed` (the last two recorded per leg and carried on the manual task, never a synchronous refusal), `authority-withdrawn` (recorded on the task slice 08's cancel-authority port raises at `pre-compensation`), `circuit-breaker-open`, `per-attempt-timeout`, `idempotency-key-conflict`, `idempotency-key-mismatch` (a pass at or below `last_pass` with no record, `inst-co-fence-guard`, D-247), `version-mismatch` | `compensation`, one entry per leg settled in the pass; `step-completion` for the pass | `retryable-on: transient` | 10 s |
| `report-outcome` | `protected` | ref + `outcome` ∈ `completed` · `failed` · `cancelled` · `superseded` · `terminal-event`, `round` (0 on first entry, else the previous answer's `nextRound`) | `reportedOutcome` (the mode actually reported — differs from `outcome` only by the §4.3 promotion `failed` → `cancelled`, or is `terminal-event` where Lifecycle already holds the order terminal, §3.6 `inst-ro-reauthorize` and `inst-ro-call` — never for `completed`, §4.9), `lifecycleCall` ∈ `acknowledged` · `already-applied` (Lifecycle refused `not-admissible` and the order read shows the report's own transition applied at this version, §4.9, decision D-188 — a settled success recorded like `acknowledged`) · `none` · `forced-terminal` (a `completed` report refused `not-admissible` while the order read shows `fulfillment_failed` with `failure_reason = operator-forced-unreconciled`, Lifecycle D-182: settled with no Lifecycle transition, and the definition takes the terminal unwind, §4.9, decision D-208) · `held` (Lifecycle refused `not-admissible` and the order read shows `on_hold` — a completed acknowledgement of a held order, Lifecycle `06 §3.6`; a settled success that records no report) · `revoke-pending` (`failed` or `cancelled` on a spawned order: Lifecycle's current grant names a generation this gear has not recorded, or a receiver has not attested its revoke; a settled success that records no report and makes no Lifecycle transition call, on which the definition presents the next `compensate-order` pass, §3.6 `inst-ro-receivers`, decision D-269), `nextRound` | step: instance-scoped `{tenant}:{correlationId}:report-outcome:{round}`; seam: lifecycle-transition `{tenant}:{orderId}:{orderVersion}:{trigger}:{round}` with `trigger` ∈ `acknowledge-completed` · `acknowledge-failed` · `cancel-workflow-mediated` ([`01 §3.3` *Rounds and attempts*](./01-foundation.md#rounds-and-attempts-the-one-rule-for-re-invokable-operations)); supersede: `{tenant}:{orderId}:{orderVersion}:control-supersede:{controlId}`, one per superseded pause control (D-269) | The set {`OrderFulfillmentCompleted`, `OrderFulfillmentAborted`}, one event per order: `OrderFulfillmentCompleted` on `completed`; `OrderFulfillmentAborted` on every unwind — all four modes, `failed`, `cancelled`, `superseded` and `terminal-event`, before or after begin-fulfillment (§3.2, decisions D-234, D-259) | none — `report-outcome` is itself the declared compensation of `begin-fulfillment` (§2.1) | `fence-not-claimed`, `outcome-not-reportable`, `version-mismatch`, `not-authorized`, `not-found`, `circuit-breaker-open`, `per-attempt-timeout`, `idempotency-key-conflict` — the Lifecycle answers they stand for are §4.9's | `step-completion` | `retryable-on: transient` | 10 s |

**Two catalogue reasons** this slice introduced are registered in `01 §4.9`: `fence-not-claimed`
(`FailedPrecondition`, 400 — `compensate-order` or a compensated `report-outcome` found no fence
row for the version, which only a definition that bypassed the fence can cause) and
`outcome-not-reportable` (`FailedPrecondition`, 400 — the fence has not verified that no active
subscription remains, or slice 04's completion predicate does not hold for `completed`, or
Lifecycle refused the evidence, the roster or a request member Orders built, §4.9). (Decision D-77: register
`fence-not-claimed` and `outcome-not-reportable` under owner `06-saga-and-compensation` in the
reason catalogue of `01 §4.9`.) A third, `outcome-already-reported` (`FailedPrecondition`, 400), is
the fence's and `authorize-cancel`'s refusal once this instance's completion report, its rejection
reflection or its unwind's own report has settled (§3.6 `inst-fence-cause`, decisions D-251, D-271).

**Per-pass deadline.** `compensate-order`'s 10 s is a per-**pass** budget, the dispatch-operation
baseline of `01 §4.2`. A pass starts a leg only while the remaining deadline covers one status
read plus one submission at the Subscriptions client's per-call budget; otherwise it settles
`in-progress` with the walk's position recorded, and the next pass resumes. A walk of any length
is therefore a sequence of bounded passes, never one unbounded call. **Per-call budgets**
(decision D-299; working baselines beside `01 §4.2`'s, each the deadline propagated on that
outbound call and never above the pass's remaining deadline): a receiver revoke 2 s, a
Subscriptions status read 2 s, a Subscriptions submission 2 s. `inst-co-revoke` sends a row's
revoke only while the remaining deadline exceeds — strictly — one revoke call plus one walk leg,
2 + 2 + 2 s, and leaves every unsent row for the next pass, which sends the rows least recently sent
first; so with every receiver down a pass sends at most two revokes and spends at most its first
4 s on them (sends at 10 s and 8 s remaining, none at 6 s), the walk still starts a leg with the
6 s left, and the receivers are retried on every pass (decision D-313).

**Why the pass is in the key.** A pass that answers `in-progress` is a settled success of *that*
key; re-issuing it would be an absorbed duplicate answering `in-progress` forever. The definition
therefore presents a new `pass` for every re-invocation and the same `pass` only for a platform
retry of the same attempt. The walk's truth is `owf_compensation_record`, not the pass: two passes
over the same subject resolve to the same compensating intent key, which Subscriptions and slice
05's registry absorb. A pass whose registry lease expired (a crash inside the pass) is settled by
the next pass, which calls `settle-from-lookup` in-process with `lookupOutcome = failure` and
`per-attempt-timeout` before starting its own effect — the looked-up truth of the expired pass is
this slice's record, which every leg writes as it settles. **The pass has its own guard**
(decision D-247). The definition presents `0` first, then each answer's `nextPass`; a pass whose
lease died answers `idempotency-lease-expired` until the definition's retry budget is spent, and
the definition then presents that pass `+ 1` — the one round it computes (`01 §4.14`). The envelope's
per-family counter would refuse that pass `idempotency-key-mismatch`, because the dead pass never
settled with success, so `compensate-order` is listed beside `begin-fulfillment` and `open-gates`
as an operation whose own guard validates its round (`01 §3.3` rule 2): the fence row keeps
`last_pass`, the highest pass presented for the run; a pass above it is admitted and recorded,
and one at or below it that the registry has no record of is `idempotency-key-mismatch`. The
settle-prior step then settles the dead pass as `failure`, and the new pass proceeds.

**Retired interface text.** The former `COMMAND compensate-order-fulfillment` and
`COMMAND run-cancellation-fence` process-engine commands are retired by ADR-0011 in favour of the
routes above; the operation names are the definition's `call` targets.

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-workflow-interface-outcome-report`

- **Contracts**: `cpt-cf-bss-orders-workflow-contract-owf-lifecycle-transition`
- **Technology**: Lifecycle SDK client, called only from inside `report-outcome`
- **Location**: `gears/bss/orders/docs/features/06-workflow-seam.md` §3.3 and §4.4 (Acknowledgement, normative)

**Endpoints Overview** (outbound, owned by Orders Lifecycle; the SDK methods of Lifecycle's
Workflow SDK contract, Lifecycle D-155, obtained through `ClientHub` — decision D-193 — each the
in-process form of the REST path named beside it):

| Method | Path | Description | Stability |
|--------|------|-------------|-----------|
| SDK | `OrdersWorkflowSeamV1::acknowledge(OrderRef, FulfillmentAcknowledgement, CallMeta)` — REST `POST /bss-orders/v1/orders/{orderId}/fulfillment-acknowledgement` | `completed { per_line_results: [{ line_id, result: activated, subscription_id }] }`, exactly one entry per line of the expected version, mapped by line id and never by position; or `failed { failure_reason, compensation_evidence, per_line_results }` with the closed `failure_reason`; called by `report-outcome` only | unstable |
| SDK | `OrdersWorkflowSeamV1::workflow_cancel(OrderRef, cancel_reason, CompensationEvidence, CallMeta)` — REST `POST /bss-orders/v1/orders/{orderId}/workflow-cancel` | Workflow-mediated cancel with compensation evidence and the cancel reason (order → `cancelled`, the Lifecycle cancel-guard exception); called by `report-outcome` only | unstable |
| `EVENT` | `OrderFulfillmentCompleted`, `OrderFulfillmentAborted` | Enqueued through the platform producer outbox in the transaction that records the reported outcome | unstable |

The evidence follows Lifecycle's closed schema (`01 §3.7` of that gear): drafts voided, activated
subscriptions rolled back, whether activation was dispatched (a settled `report-spawn-signal` for
the version, slice 05), whether at-sale facts had been emitted, and
`no_active_subscription_remains = true`. `report-outcome` builds it from
`owf_compensation_record` and `owf_cancellation_fence` inside Orders; it never crosses the engine
boundary. The Lifecycle endpoints are marked `unstable` deliberately: the activated-cancel leg's
cancellation reason is `order_compensation` (§4.6), answered in Subscriptions PR #5131, which is
still open, so the payload of the compensating intent underneath these reports is fixed only once
it merges.

### 3.4 Internal Dependencies

| Dependency Gear | Interface Used | Purpose |
|-------------------|----------------|----------|
| orders-workflow (slice 01) | Step envelope, `settle-from-lookup`, `owf_process_instance` phase projection | Every operation runs inside the envelope; `run-cancellation-fence` writes the `compensating` phase; a crashed pass is settled through `settle-from-lookup` |
| orders-workflow (slice 03) | Closure port `close_open_approvals(correlationId, reason)` ([`03 §3.2`](./03-approval-execution.md#32-component-model)) | Fence step 1: cancel every `planned` or `open` gate and close an open park with `resolution = order-terminated`, inside `run-cancellation-fence`'s unit of work |
| orders-workflow (slice 05) | Intent port; `reconcile-intent`; `owf_provisioning_intent` | Submit compensating intents; reconcile in-flight intents in fence step 3; read the forward ordinal and unmatched confirmations |
| orders-workflow (slice 07) | Manual-task creation port; closure port `close_open_tasks` scoped to `lifetime-ceiling-reached` ([`07 §3.2`](./07-manual-tasks.md#32-component-model)); Incident Recorder; verified-override record | Escalate a failed leg (always a manual task); close the ceiling task in fence step 1 (D-149); record fail-fast forward failures as incidents; enumerate override-attached subjects |
| orders-workflow (slice 08) | Cancel request record (`owf_cancel_request`, inserted by slice 09's Control Operation Gateway, read by `authorize-cancel`); suspension closure port `close_suspension(correlationId, closedReason)`; cancel-authority port `recheck_cancel_authority(correlationId, cancelRequestRef, point)` ([`08 §3.2`](./08-hold-and-cancel.md#32-component-model)) | The cancel reason `report-outcome` carries to `workflow-cancel`; fence step 1 closes a suspension on an unwind taken from hold; `compensate-order` re-checks authority at `pre-compensation` on the cancel trigger |
| Subscriptions | Provisioning-intent SDK client (through slice 05's port) | Submit draft-void and activated-cancel compensating intents; resolve a subject's current phase; never call OSS directly |
| Orders Lifecycle | Outcome-report contract (§3.3), bound by reference to `06-workflow-seam.md` §4.4 | Report `completed`, `fulfillment_failed` or the workflow-mediated cancel |

**Dependency Rules** (per project conventions):
- No circular dependencies
- Always use sdk modules for inter-gear communication
- No cross-category sideways deps except through contracts
- Only integration/adapter gears talk to external systems
- `SecurityContext` must be propagated across all in-process calls

### 3.5 External Dependencies

This slice introduces no new external-system dependency beyond what slice 05 already declares.
Compensation reaches OSS only transitively, through Subscriptions. The platform definition runtime
is not an external dependency of these operations: it calls them, they call nothing on it
(`01 §3.4`).

#### Subscriptions (external aggregate, canonical owner of subscription state)

- **Contract**: `cpt-cf-bss-orders-workflow-contract-owf-provisioning-intent`

| Dependency Gear | Interface Used | Purpose |
|-------------------|---------------|----------|
| Subscriptions | draft-void / activated-cancel provisioning intent; subscription status read | Compensate a completed wave-1 create or wave-2 activation; resolve a subject's current phase |

**Dependency Rules** (per project conventions):
- No circular dependencies
- Always use SDK modules for inter-gear communication
- No cross-category sideways deps except through contracts
- Only integration/adapter gears talk to external systems
- `SecurityContext` must be propagated across all in-process calls

### 3.6 Interactions & Sequences

Every sequence below is **definition task → operation → record**. The definition text is the
canonical YAML of [`10 §3.6`](./10-process-definition.md#36-interactions--sequences) fragment (c)
`compensateOrder`, fragment (d) `cancelPath` and fragment (f) `supersede`; it is not repeated
here. What follows is what happens **inside** each operation.

#### Order-Level Failure Compensation

**ID**: `cpt-cf-bss-orders-workflow-seq-failure-compensation`

**Use cases**: `cpt-cf-bss-orders-workflow-usecase-owf-partial-failure` (PRD §Acceptance Criteria 9)

**Actors**: `cpt-cf-bss-orders-workflow-actor-owf-orders-lifecycle`, `cpt-cf-bss-orders-workflow-actor-owf-subscriptions`

```mermaid
sequenceDiagram
    participant PL as serverless-runtime (fragment c, compensateOrder)
    participant CF as run-cancellation-fence
    participant CE as compensate-order
    participant SUB as Subscriptions (via 05 port)
    participant MT as Manual-Task Creator (07)
    participant OR as report-outcome
    participant LC as Orders Lifecycle
    PL ->> CF: trigger failure
    CF ->> CF: resolve the cause, map its failure_reason (§4.8); claim owf_cancellation_fence; phase → compensating; closure port (03); freeze owf_receiver_revoke rows on a spawned order (sends nothing); stamp steps 1, 2
    CF -->> PL: fenceRef, claim = claimed
    loop pass = 0, 1, ... until complete
        PL ->> CE: pass n
        CE ->> SUB: revoke each unattested receiver (spawned order, D-269)
        CE ->> CE: step 3: reconcile identified intents (05 reconcile-intent); freeze subjects once terminal
        CE ->> SUB: walk: resolve phase; draft_void | activated_cancel | already-absent, one subject at a time
        alt leg bound reached
            CE ->> MT: create task, leg-specific reason (either policy)
            CE ->> CE: record failed-pending-escalation; CONTINUE (lines are independent, D-196)
        end
        CE ->> LC: step 5: read the current grant (spawned order); record an unrecorded generation beside a predecessor pending revoke, its source derived from the predecessor until the read names it; freeze rows for any generation not yet frozen (D-269, D-288, D-291, D-296, D-298)
        CE -->> PL: compensationState, nextPass, taskRefs
        Note over PL: in-progress / pending-escalation → awaitCompensationResolution, then pass n+1
    end
    PL ->> OR: outcome failed
    OR ->> LC: inst-ro-receivers: read the current grant (spawned order)
    alt unrecorded generation, generation without rows, or unattested row
        OR -->> PL: lifecycleCall = revoke-pending, nextRound (no report)
        Note over PL: one more compensate-order pass, then report-outcome under nextRound
    else receivers revoked
        OR ->> LC: supersede_fulfillment_control per superseded control (ControlRevokeEvidence from the rows; Lifecycle `bss/orders-stage4` D-259)
    end
    OR ->> OR: gate: no_active_verified_at set; build evidence from records
    OR ->> LC: fulfillment-acknowledgement (acknowledge-failed, evidence, failure_reason)
    OR ->> OR: enqueue OrderFulfillmentAborted in the recording transaction
    OR -->> PL: reportedOutcome failed
    PL ->> PL: terminate-instance (01), terminationKind compensated
```

**Description**: The definition enters `compensateOrder` from the partial-failure arm (remediation
exhausted under `remediate`, or immediately under `fail-fast` on a typed line failure — at once also
while an unresolved-refusal task is open, decision D-300), from an unresolved-refusal task's
`binding-expired` answer once the plan's `activation_deadline_at` has elapsed (`07 §4.2`, D-294), or
from the pre-activation abort (slice 04). An unresolved activation refusal is never itself the
entry: its task is opened under either policy and is never exhausted (§4.8, D-285, D-304). *Remediation exhausted* is **3 failed operator resolution attempts on the same task, or
the manual-task SLA deadline elapsing without resolution, whichever comes first** (D-55), never on
an unresolved-refusal task; it is
`resolve-manual-task`'s `exhausted` answer (slice 07) on which fragment (c) routes. A
`pending-escalation` answer never reaches `report-outcome`: the order stays non-terminal until a
later pass answers `complete`.

**Algorithm: Run Cancellation Fence** (inside `run-cancellation-fence`, after key resolution)

Input: ref, `trigger`, `cancelRequestRef`, `triggerEventId`
Output: `fenceRef`, `claim`, `effectiveTrigger`, `inFlightCount`

1. [ ] - `p1` - Lock the instance row; **IF** `terminal_outcome` is set: **RETURN** `permanent-failure` with `version-mismatch` - `inst-fence-lock-instance`
2. [ ] - `p1` - **Cause precondition** (decision D-106). **IF** the record holds a settled `report-outcome` `completed` of this instance answering `lifecycleCall` `acknowledged` or `already-applied`, a settled `reflect-verdict` of this instance answering `rejected`, or a fence row for the version with `reported_at` set: **RETURN** `permanent-failure` with `outcome-already-reported`, claiming and absorbing nothing — Lifecycle holds the order `completed` or `rejected`, or the run's own report has settled and its event is enqueued, so no trigger may unwind it again; the instance liveness pass terminates such an instance instead of offering cancel (`01 §3.8` item 2, decisions D-251, D-271). A `forced-terminal` answer is not a completion and stays the `terminal-event` cause below. Otherwise the record **MUST** already hold the cause the trigger names, else **RETURN** `permanent-failure` with `not-found`, claiming nothing: `cancel` — a settled `authorize-cancel` answering `authorized = true` for `cancelRequestRef` on this instance (slice 08); `supersede` — a settled `admit-trigger` listen admission `supersede` for `triggerEventId` on this instance (slice 02); `terminal-event` — a settled `terminate-on-terminal-event` for `triggerEventId` answering `terminate = true` (slice 02), or, with `triggerEventId` null, a settled `report-outcome` of this instance answering `lifecycleCall = forced-terminal` (Lifecycle's operator-forced failure overtook the completion report, §4.9, decision D-208); `failure` — a failure the unwind may act on: a manual task of this instance resolved `exhausted` ([`07 §4.2`](./07-manual-tasks.md#42-remediation-exhausted-and-the-consequence-of-a-breach-normative)), or under `fail-fast` a forward line of the frozen plan in `failed` whose failing intent is not recorded `refusalKind = unresolved` (an unknown code, a denial or an outage stays unresolved and is never a cause, §4.8, D-285), or a `draft_create` intent of the version carrying `external_activation_observed_at` (the re-read found its subscription active without an activation of this gear, `05 §4.4`, decision D-233; reason `draft-activated-externally`), or the plan's `activation_deadline_at` elapsed against database time (the wave-2 guard's `bindingExpired` answer, `05 §3.6`, decision D-194, or the SLA check's `binding-expired` answer on an open unresolved-refusal task, `07 §4.2`, decision D-294), or an activation intent of the version recorded `failed` with `refusalKind = verified-expiry` (a typed verified expiry, closure or temporary end, `05 §3.3`; reason `order-binding-expired`, decision D-257), or a settled `replace-fulfillment-grant` answering `refused` (Lifecycle refused the D-201 replacement, `05 §3.3`; decision D-243; reason `grant-replacement-refused`), or a settled `re-check-pre-activation` answer that aborts ([`04 §4.2`](./04-fulfillment-plan.md#42-the-re-check-advisory-at-construction-authoritative-before-wave-2)); on `failure` the cause found also names the Orders reason, which §4.8 maps to Lifecycle's `failure_reason`, and the first cause in this list order is the one recorded. This is the pattern `terminate-on-terminal-event` already follows (`not-found` without a settled `terminate` admission, [`02 §3.3`](./02-triggers-and-start.md#33-api-contracts)); with the invocation binding of `01 §3.3` step 3 it is what keeps a caller that is not the instance's own invocation, or a definition that misorders the path, from unwinding a healthy order - `inst-fence-cause`
3. [ ] - `p1` - **TRY** insert `owf_cancellation_fence` for `(order_id, order_version)` with `trigger`, `correlation_id`, the references for this trigger — on `failure`, `orders_failure_reason` and `failure_reason` from step 2 and §4.8 — and `claimed_by_attempt_id = attemptId`; **CATCH** unique violation: apply the absorption table of §4.3 to the existing row, increment `absorbed_trigger_count`, write `step-completion`, **RETURN** `claim = absorbed` with the row's `effectiveTrigger` and `inFlightCount` - `inst-fence-claim`
4. [ ] - `p1` - **Step 1.** Move the phase projection `started`/`suspended`/`parked` → `compensating` (`01 §3.7`; a parked instance, including one parked at the lifetime ceiling, is unwound only through this fence, D-82), after which every dispatch operation of slice 05 refuses for the version; call slice 03's closure port `close_open_approvals(correlationId, reason = trigger)` in this unit of work, which sets every `planned` or `open` gate to `cancelled` and closes an open park with `resolution = order-terminated`; call slice 08's suspension closure port `close_suspension(correlationId, closedReason)` in the same unit of work, with `closedReason` = `cancelled-from-hold` (`cancel`), `superseded-from-hold` (`supersede`) or `terminated-from-hold` (`failure`, `terminal-event`), which closes an `open` or `resume_ahead` suspension row and clears `owf_process_instance.suspended` ([`08 §3.7`](./08-hold-and-cancel.md#37-database-schemas--tables)); call slice 07's closure port `close_open_tasks(correlationId, outcome = closed-by-unwind, reason = lifetime-ceiling-reached)` in the same unit of work, which closes an open ceiling task and its `requested` request, so the ceiling's exclusivity rule (`07 §4.4`) does not freeze the unwind's compensation tasks at `escalate` and the unwind's resolution wait cannot take that task's `retry` for a compensation retry (decision D-149); stamp `dispatch_stopped_at`. **Receivers** (decisions D-243, D-248, D-269): in the same unit of work, insert one `owf_receiver_revoke` row (§3.7) for each generation of the version in `owf_fulfillment_grant` whose `close_kind` is not `revoked` (open, `paused` or `closed`) and each receiver of that generation's roster, under the revoke key `{tenant}:{orderId}:{orderVersion}:receiver-revoke:{generation}:{receiverRef}` — its unique constraint absorbs a replay — stamp each such generation's `revoke_frozen_at` through slice 05's grant port, which takes it out of the one-open-grant index, pending its revoke (`05 §3.7`, D-291), and mark every pause control of slice 08 for the version that is `staged` or `pausing` `superseded` (`08 §3.6` `inst-rp-resolve`). Nothing is sent: the operation is record-only and calls nothing outside Orders' database; `compensate-order` sends the revokes (`inst-co-revoke`) - `inst-fence-step1`
5. [ ] - `p1` - **IF** `trigger = failure`: hand each forward line that is failed, or whose current forward intent is recorded `unresolved` (an unresolved refusal, `refusalKind = unresolved`, or an intent past the sweep floor, `05 §3.6` `inst-ri-ladder`), and that has no open manual task to slice 07's Incident Recorder in this unit of work — under `fail-fast` always; under `remediate` only when the record holds one of the causes the definition routes to the unwind under either policy with no task: a `draft_create` intent carrying `external_activation_observed_at` (`draft-activated-externally`), or `order-binding-expired` from the wave-2 guard's elapsed `activation_deadline_at` or an activation intent recorded `refusalKind = verified-expiry` (`10 §3.6` (b), D-194, D-233, D-257). A line with an open manual task is skipped — its task, closed by `terminate-instance`, is its object, never both; a line listed in the same answer as the failure that ends the order never got its task and is handed over with the others (decisions D-300, D-330); never for a compensation leg - `inst-fence-failfast-incident`
6. [ ] - `p1` - **Step 2.** Select every `owf_provisioning_intent` row for the version that is accepted and not terminal, and every slice-05 dispatch registry record for the version that is `in_flight` or `open`; record their count and stamp `in_flight_identified_at` - `inst-fence-step2`
7. [ ] - `p1` - Write `phase-transition` (`phase_from` → `compensating`) and settle; **RETURN** `claim = claimed`, `effectiveTrigger = trigger`, `inFlightCount` - `inst-fence-return`

**Algorithm: Compensate Order** (inside `compensate-order`, after key resolution)

Input: ref, `pass`
Output: `compensationState`, `nextPass`, `taskRefs[]`

1. [ ] - `p1` - Lock the fence row for `(order_id, order_version)`; **IF** none: **RETURN** `permanent-failure` with `fence-not-claimed`. **Pass guard** (decision D-247): **IF** `pass` is at or below the row's `last_pass` — a pass the registry holds no record of, since a replay of a presented pass is resolved before this step — **RETURN** `permanent-failure` with `idempotency-key-mismatch`; **ELSE** set `last_pass = pass` in this unit of work - `inst-co-fence-guard`
2. [ ] - `p1` - **IF** a previous pass's registry record is `in_flight` with a dead lease: settle it through `settle-from-lookup` (`lookupOutcome = failure`, `per-attempt-timeout`, the record's `lease_holder`) in-process before continuing; the dead pass is thereby closed, and this pass, admitted by step 1 above it, proceeds — a later presentation of the dead pass replays its settled failure (D-247) - `inst-co-settle-prior`
3. [ ] - `p1` - **Revoke the receivers** (decisions D-248, D-269, D-299). **FOR EACH** `owf_receiver_revoke` row of the version with `attested_at` null, in ascending `last_sent_at` with never-sent rows first, **WHILE** the pass's remaining deadline exceeds — strictly — one revoke call plus one walk leg (one status read and one submission at the per-call budgets of §3.3 *Per-pass deadline*; D-313): send its receiver the revoke for the plan's attempt and the row's generation under the row's `revoke_key`, each call under the 2 s receiver per-call budget, and increment `send_count` and set `last_sent_at`; on an attestation the receiver marks complete, write `attestation` and `attested_at` once; an unavailable or silent receiver, or an attestation marked incomplete, leaves the row unattested — never inferred revoked — for the next pass; a row the budget left unsent keeps its `send_count` and is sent first by the next pass, so a receiver outage costs a pass at most its revoke share and never the walk. It runs before `inst-co-reauthorize`: a revoke only extends the fence `run-cancellation-fence` already committed and destroys nothing, so it is not a submission the re-check guards (D-268 item 8 as amended by D-269) - `inst-co-revoke`
4. [ ] - `p1` - **Re-check before the first downstream submission** (decision D-268). **IF** the fence row's `trigger` is `cancel` **AND** nothing of the run has been submitted downstream — no `SUB-O12` withdrawal of *Step 3* (`inst-co-step3`) and no compensating leg; *Step 3*'s reads are not submissions, nor are the revokes of `inst-co-revoke`: call slice 08's cancel-authority port `recheck_cancel_authority(correlationId, cancelRequestRef, pre-compensation)` ([`08 §3.2`](./08-hold-and-cancel.md#32-component-model)); **IF** it answers `withdrawn`: stamp `reauthorization_required_at` on the fence row, settle, **RETURN** `pending-escalation` with the port's `authority-withdrawn` task in `taskRefs`, `nextPass = pass + 1`; **IF** `reauthorization_required_at` is set: **RETURN** the same without submitting a withdrawal or a leg; a PDP outage is `retryable-failure` - `inst-co-reauthorize`
5. [ ] - `p1` - **Step 3.** **IF** `reconciliation_complete_at` is null: for each intent step 2 identified that is not terminal, call slice 05's `reconcile-intent` unit in-process (a late success is a created subscription); **IF** any remains non-terminal inside the sweep floor: settle, **RETURN** `in-progress`, `nextPass = pass + 1` — an accepted in-flight intent is waited out, never re-submitted (`SUB-O12`, §4.6); **IF** any forward intent of the version is `unresolved` (the sweep floor reached with no known outcome, [`05 §4.2`](./05-provisioning-intents.md#42-reconciliation-sweep-schedule-working-baseline)): it is **not** reconciled, and the floor is no answer — submit its withdrawal once through slice 05's intent port (`SUB-O12`, §3.6 *Upstream dependency answered*), recording the withdrawal intent on the row: `withdrawn` is a terminal failure that created nothing; a refusal because the request was already applied names the subscription it created, which `reconcile-intent` records on the row and the freeze of step 6 lists as a subject; while the withdrawal itself has no known outcome, settle and **RETURN** `in-progress`; once the withdrawal has reached its own sweep floor or 5 submission failures, call slice 07's manual-task creation port with reason `intent-unresolved` for the line in this unit of work, record `manual_task_id` on the intent and **RETURN** `pending-escalation` with the task in `taskRefs`, `nextPass = pass + 1` — the order stays non-terminal, and an operator's `retry` re-arms the row for one on-demand read (`05 §3.7`) without submitting anything; `reconciliation_complete_at` is stamped only when no forward intent of the version is `submitted` or `unresolved`, because an unreadable accepted create may be a live subscription and reporting the order cancelled over it would assert to Lifecycle what this gear has not verified (decision D-212); **ELSE** stamp `reconciliation_complete_at` - `inst-co-step3`
6. [ ] - `p1` - **Freeze.** **IF** no `owf_compensation_record` exists for the run: insert one row per subject — every subscription created for the version (including late successes and slice 05's unmatched confirmations recorded against it) plus every override-attached subscription slice 07 verified — with `compensation_sequence` from `execution_seq` (or, for an override-attached subscription, the task's `override_execution_seq`, the next per-order ordinal minted at verification, D-268), `outcome` null; the `UNIQUE (order_id, order_version, compensation_sequence)` constraint makes the freeze total and a replay of it an absorbed no-op - `inst-co-freeze`
7. [ ] - `p1` - **Step 4, the walk.** **FOR EACH** record with `outcome` not `succeeded`, in descending `compensation_sequence`, while the remaining deadline covers one leg: - `inst-co-walk`
   1. [ ] - `p1` - Every subject is walked on its own: acquisition lines are independent (decision D-196), so a record whose `outcome = failed-pending-escalation` holds no other leg back and nothing is skipped here - `inst-co-blocked`
   2. [ ] - `p1` - **IF** the record already names a compensating intent that is accepted and not terminal: call `reconcile-intent` in-process; **IF** still non-terminal inside the sweep floor: **STOP** the walk for this pass (the walk is sequential); **IF** it reached the floor: go to 7.6 - `inst-co-reconcile-leg`
   3. [ ] - `p1` - **IF** the record names a compensating intent Subscriptions settled terminal `failed` under the record's current `comp_attempt` (decision D-258): it counts toward the bound — increment `submission_failure_count`; at the bound go to 7.6, below it **STOP** the walk for this pass. Nothing is resubmitted under that key, which `SUB-O11` would answer with the stored failed request; only the compensation task's `retry` mints the next `comp_attempt` (`07 §4.4`), under which 7.5 submits again - `inst-co-failed-leg`
   4. [ ] - `p1` - Resolve the subject's current phase through Subscriptions; **IF** the read fails or times out: leave the record unsettled and **STOP** the walk for this pass - `inst-co-resolve`
   5. [ ] - `p1` - **MATCH** the phase: `draft` → submit `draft_void`; `activated` → submit `activated_cancel`, through slice 05's intent port under the compensating key (§2.1), recording the intent on the record; **IF** not accepted: increment `submission_failure_count` and, below the bound, **STOP** for this pass; absent → set `resolution_mode = already-absent`, `outcome = succeeded` - `inst-co-leg`
   6. [ ] - `p1` - **IF** a bound was reached (sweep floor or `submission_failure_count` = 5, terminal failures of 7.3 included): set `outcome = failed-pending-escalation`, `failure_reason` = the leg-specific value; call slice 07's manual-task creation port with that reason in this unit of work and record `manual_task_id`; **CONTINUE** the walk - `inst-co-escalate`
   7. [ ] - `p1` - On a confirmed compensating outcome: set `outcome = succeeded`, `resolved_at`, `attempt_id`; write a `compensation` audit entry - `inst-co-settle-leg`
8. [ ] - `p1` - **IF** every subject has a settled outcome: stamp `all_compensated_at`; **IF** every outcome is `succeeded`: on a version with a recorded grant, first require the receivers (decisions D-243, D-248, D-269, D-288, D-296, D-298) — read Lifecycle's current grant (`…-upreq-lifecycle-current-grant-read`); an unavailable read settles **RETURN** `in-progress` with `nextPass = pass + 1`. **IF** it names a generation of the version this gear has not recorded — a successor a post-spawn resume or a D-201 replacement committed during the unwind, which Lifecycle admits because it learns of the unwind only from `report-outcome` — first give every recorded generation of the version not `revoked` whose `revoke_frozen_at` is null its rows, as below, then record it through the grant port `record_grant` with the `predecessor_grant_id` the read names and the `source` it names — `spawn`, `resume` or `replace` (`…-upreq-lifecycle-current-grant-read` as amended by D-291), never a fixed value. Until the read names the source, derive it from the predecessor the read names, as this gear recorded it (decisions D-296, D-313): closed `paused`, or `revoked` with `revoked_from = paused` → `resume`, because `settle-receiver-pause` records the generation `paused` (`08 §3.6` `inst-rp-evidence`) before it reports the control to Lifecycle (`inst-rp-report`), so Lifecycle's resume always finds its predecessor recorded here as paused — closed `paused`, or, once an earlier pass of this step has revoked it, `revoked` with `revoked_from = paused`, the rewrite never losing the pause; any other state — in practice `closed_at` null, whatever its `revoke_frozen_at`, or `revoked` from `open` or `closed` — → `replace`, because `replace-fulfillment-grant` closes its predecessor `closed` and records the successor in one unit of work after Lifecycle's commit (`05 §3.6` *Algorithm: replace-fulfillment-grant*), so only a replacement whose local unit did not run — refused on the `compensating` phase, or lost to a crash — leaves its predecessor unclosed. The predecessor is always recorded here: a generation Lifecycle commits needs this gear's replacement call or its reported pause, neither of which starts once the version is `compensating` (every dispatch operation refuses, and the control intake records a control `superseded` and signals nothing, `08 §3.3`), so at most one generation is late. The `source` is the record's provenance and no input of the revoke: whatever it is, the generation is recorded, gets its rows below and is revoked like any other, and this step never refuses it — a refusal here would be caught by the definition's transient catch as `retry-exhausted` and retried silently with no task raised (`10 §3.6` (c)). An unclosed predecessor is never closed `closed` here — `closed` is a closure for a replacement written from receiver attestations, never from absence (`05 §3.7`): it keeps `closed_at` null, pending revoke (`05 §3.7`) through its frozen `owf_receiver_revoke` rows, and is recorded `revoked` below once they are all attested; the one-open-grant index of `05 §3.7` excludes a row whose revoke rows are frozen (`revoke_frozen_at`), so the successor is recorded beside it (D-291). Then **IF** any recorded generation of the version not `revoked` — Lifecycle's current one included — has `revoke_frozen_at` null, because it was recorded after the fence froze the rows (by this step, by `apply-resume`'s `inst-grant-reconcile`, or by a late `report-spawn-signal` or `replace-fulfillment-grant`), insert one row per receiver of its roster and stamp its `revoke_frozen_at` as `inst-fence-step1` does, settle and **RETURN** `in-progress` with `nextPass = pass + 1`, the next pass's `inst-co-revoke` sending them. The test is `revoke_frozen_at`, never whether rows exist (decision D-298): an unfrozen generation is never read as attested, and a frozen generation whose roster names no receiver has no rows and is vacuously revoked — nothing to send, nothing to attest, its part of the barrier evidence the digest over the empty set. **IF** any `owf_receiver_revoke` row of the version is unattested (a receiver outage), settle and **RETURN** `in-progress` with `nextPass = pass + 1`, never `complete`. Record each frozen generation whose rows are all attested (none, for an empty roster) and whose grant is not yet `revoked` as `revoked` through slice 05's grant port (a row closed `paused` or `closed` rewritten `revoked` once), writing once its `revoked_from` — the earlier `close_kind`, or `open` for a row whose `closed_at` was null (D-313) — and store `receiver_barrier_digest`, the digest of the rows' attestations. Then stamp `no_active_verified_at` (**Step 5**) unless a previous pass has, **RETURN** `complete` with `nextPass = pass + 1`, which only a later `revoke-pending` answer of `report-outcome` presents (D-269) - `inst-co-step5`
9. [ ] - `p1` - **IF** every subject has a settled outcome and any is `failed-pending-escalation`: **RETURN** `pending-escalation` with `taskRefs`; **ELSE RETURN** `in-progress`; `nextPass = pass + 1` in both - `inst-co-return`

**Algorithm: Report Outcome** (inside `report-outcome`, after key resolution)

Input: ref, `outcome`, `round`
Output: `reportedOutcome`, `lifecycleCall`, `nextRound`

1. [ ] - `p1` - **IF** `outcome = completed`: **IF** a fence row exists for the version, or slice 04's completion predicate does not hold: **RETURN** `permanent-failure` with `outcome-not-reportable`; **ELSE** read the per-line subscription identifiers from Orders' record and go to step 4 with trigger `acknowledge-completed` - `inst-ro-completed`
2. [ ] - `p1` - Read the fence row; **IF** none: **RETURN** `permanent-failure` with `fence-not-claimed`; **IF** `no_active_verified_at` is null: **RETURN** `permanent-failure` with `outcome-not-reportable` - `inst-ro-gate`
3. [ ] - `p1` - Take the mode from the fence row's `trigger`; **IF** it differs from `outcome` other than by the §4.3 promotion `failure` → `cancel`: **RETURN** `permanent-failure` with `version-mismatch`; build the evidence from `owf_compensation_record`, and — in **every** mode — `retainedObligations[]`: every line's binding reference from the frozen plan (empty where no plan froze) with the subject outcomes and `fulfillmentBegun` (the plan row's `begin_fulfillment_committed_at` set), stating that this gear voided or cancelled only what it created and **released nothing else**: no reservation, no accepted binding, no payment; it travels on `OrderFulfillmentAborted` in every mode and, where a Lifecycle call is made, with that call too, so Lifecycle (capture versus cancellation) and Billing receive it whether or not this gear calls Lifecycle (D-227, D-234); **MATCH** the mode: `failure` → trigger `acknowledge-failed` with the row's mapped `failure_reason` (§4.8); `cancel` → trigger `cancel-workflow-mediated` with the `cancel_reason` of the request `cancel_request_ref` names ([`09 §3.7`](./09-read-and-authz.md#37-database-schemas--tables) `owf_cancel_request`); `supersede` or `terminal-event` → no Lifecycle call, go to step 7 - `inst-ro-mode`
4. [ ] - `p1` - **Only from fulfillment, and no re-check** (decisions D-109, D-84 as amended). This operation runs no cancel-authority re-check: the walk has verified that no active subscription remains, so the submission records a fact rather than performing a destructive act. **IF** the mode is `failure` or `cancel` **AND** the plan row's `begin_fulfillment_committed_at` is null: make no Lifecycle call, because Lifecycle has no Workflow transition out of a state before `in_fulfillment` ([Lifecycle `01 §4.3`](../../../orders/docs/features/01-foundation.md#contract-01-4-3) rows 14, 16, 26, 27); read the order through the Lifecycle PDP-authorized order read; **IF** it is terminal, go to step 7 with `reportedOutcome = terminal-event` and `lifecycleCall = none`; **ELSE RETURN** `permanent-failure` with `outcome-not-reportable` — no rule reaches it, since `authorize-cancel` admits a cancel before fulfillment only for an order Lifecycle already holds terminal ([`08 §3.6`](./08-hold-and-cancel.md#36-interactions--sequences)) and a plan-level failure passes `begin-fulfillment` first ([`04 §4.3`](./04-fulfillment-plan.md#43-plan-level-failures)) - `inst-ro-reauthorize`
5. [ ] - `p1` - **Receivers and superseded pause controls** (decisions D-248, D-269), for `failure` or `cancel` on a version with a recorded grant: read Lifecycle's current grant (`…-upreq-lifecycle-current-grant-read`); **IF** it names a generation of the version this gear has not recorded, **OR** any recorded generation of the version not `revoked`, Lifecycle's current one included, has `revoke_frozen_at` null — its rows not frozen; a frozen generation with no rows, an empty roster, is vacuously revoked (D-288, D-298) — **OR** any `owf_receiver_revoke` row of the version is unattested: settle with no report recorded and no Lifecycle transition call and **RETURN** `lifecycleCall = revoke-pending` with `nextRound` and no `reportedOutcome` — the definition presents the next `compensate-order` pass, whose step 5 records the generation and whose revokes follow, and then this operation under `nextRound` (`10 §3.6` (c)); an unavailable read is `retryable-failure`. Then, **FOR EACH** pause control of slice 08 for the version in `superseded` with `lifecycle_superseded_at` null, once Lifecycle offers `supersede_fulfillment_control` (`…-upreq-lifecycle-control-supersede-by-revoke`; `bss/orders-stage4` D-259): call it under `{tenant}:{orderId}:{orderVersion}:control-supersede:{controlId}` with the control's `ControlRevokeEvidence` built from the `owf_receiver_revoke` rows of the control's generation — order, version, attempt, generation, roster digest and, per roster receiver, its `fence_id`, `revoked_generation` and `revoked_at`; `superseded`, `already_superseded` and `not_pending` stamp `lifecycle_superseded_at` on the control; unavailability, a timeout or `still-processing` is `retryable-failure`; `control-generation-mismatch`, `control-not-supersedable`, `control-supersede-evidence-mismatch` and `version-conflict` are a defect of this gear's record, `permanent-failure` with `outcome-not-reportable`, which faults the invocation (§4.9). Until Lifecycle offers the operation the control stays pending there, and step 6 meets `still-processing`, the documented residual - `inst-ro-receivers`
6. [ ] - `p1` - Call the Lifecycle endpoint under the lifecycle-transition key `{tenant}:{orderId}:{orderVersion}:{trigger}:{round}` with the fence's effective deadline — for `acknowledge-failed` and `workflow-cancel` on a spawned order carrying the receiver barrier evidence, built from the version's attested `owf_receiver_revoke` rows and summarized by `receiver_barrier_digest` (D-269), beside the compensation evidence (Lifecycle stages the terminal and finalizes it on that evidence, answering `still-processing` until it does, D-243) — and answer every outcome by the table of §4.9: a committed or replayed outcome goes to step 7; unavailability, `still-processing` and `authorization-context-changed` are `retryable-failure`; `not-admissible` on an order the read shows at `orderVersion` in the trigger's own target state — `completed` for `acknowledge-completed`, with the per-line read (Lifecycle `GET …/orders/{orderId}/lines`) showing every line `activated` with the subscription identifier this report carries, or `fulfillment_failed` for `acknowledge-failed` — is this report applied before Lifecycle's ≥ 30-day workflow-class receipt floor (D-203) closed and goes to step 7 with `reportedOutcome` = the mode and `lifecycleCall = already-applied` ([`01 §3.3` *Rounds and attempts*](./01-foundation.md#rounds-and-attempts-the-one-rule-for-re-invokable-operations) rule 4, decision D-188); `not-admissible` on an order the Lifecycle PDP-authorized order read shows `on_hold` answers `lifecycleCall = held` with `nextRound` for `completed`, settling and stamping nothing — the definition waits for the resume and reports again under the next round ([`01 §3.3` *Rounds and attempts*](./01-foundation.md#rounds-and-attempts-the-one-rule-for-re-invokable-operations) rule 4); `not-admissible` on an order the read shows terminal goes to step 7 with `reportedOutcome = terminal-event` and `lifecycleCall = none` for `failed` or `cancelled`, and answers `permanent-failure` with `version-mismatch` for `completed`, which cannot meet a terminal order (§4.9, decision D-166); every other refusal is the `permanent-failure` §4.9 names - `inst-ro-call`
7. [ ] - `p1` - In one transaction: stamp `reported_at` and `reported_outcome` (the `reportedOutcome`) on the fence row (or, for `completed`, on the step record), enqueue the declared event where §3.2's table names one for the `reportedOutcome`, write `step-completion`, settle; **RETURN** `reportedOutcome`, `lifecycleCall` and `nextRound` - `inst-ro-record`

#### Authorized-Cancellation Compensation

**ID**: `cpt-cf-bss-orders-workflow-seq-cancellation-compensation`

**Use cases**: `cpt-cf-bss-orders-workflow-usecase-owf-cancel-with-rollback` (PRD §6.4)

**Actors**: `cpt-cf-bss-orders-workflow-actor-owf-orders-lifecycle`, `cpt-cf-bss-orders-workflow-actor-owf-subscriptions`

```mermaid
sequenceDiagram
    participant PL as serverless-runtime (fragment d, cancelPath)
    participant AC as authorize-cancel (08)
    participant CF as run-cancellation-fence
    participant CE as compensate-order
    participant OR as report-outcome
    participant LC as Orders Lifecycle
    PL ->> AC: cancelRequestRef
    AC -->> PL: authorized = true (apply-time re-check done)
    PL ->> CF: trigger cancel, cancelRequestRef
    CF -->> PL: claimed | absorbed (a failure-path run is promoted, §4.3)
    loop until complete
        PL ->> CE: pass n
        CE -->> PL: compensationState
    end
    PL ->> OR: outcome cancelled
    OR ->> LC: workflow-cancel (cancel-workflow-mediated, evidence, cancel reason)
    OR ->> OR: enqueue OrderFulfillmentAborted in the recording transaction
    PL ->> PL: terminate-instance (01)
```

**Description**: An authorized cancellation is gated by the same five fencing steps, so a
subscription accepted while cancellation was underway is still discovered and rolled back. The
fence row is claimed before step 1, so a second cancellation, or one arriving while a failure-path
run is walking, absorbs against the existing run (§4.3). The workflow-mediated cancel is admitted
by Lifecycle from `in_fulfillment` and from `on_hold` with pre-hold `in_fulfillment`, before the
spawn signal as after it, and always requires evidence (Lifecycle D-134). A cancel request for an
order not yet in fulfillment is Lifecycle's ordinary cancel, which reaches this process as
`OrderCancelled` on the terminal-event path of fragment (f), not through this sequence: the
Workflow cancel route refuses it with `action-not-offered` (`09 §3.3`) and `authorize-cancel`
answers `preFulfillment` for it (`08 §3.6`, decision D-109). The reason Lifecycle requires on
`workflow-cancel` is the `cancel_reason` the route recorded on the request (`09 §3.7`); on a
promoted failure run it is the promoting request's.

#### Supersession and Terminal-Event Unwind

The supersede and terminal-event paths of fragment (f) call the same three operations with
`trigger: supersede` or `trigger: terminal-event`, after `admit-trigger` (`supersede`) or
`terminate-on-terminal-event` (slice 02). `report-outcome` then makes **no Lifecycle call** —
`superseded` because the order is live at the new version, `terminal-event` because the order is
already terminal (`02 §4.7` rule 5). On supersession the run is pre-fulfillment, so
`compensate-order`'s walk is normally empty and the fence reduces to the closure port and the
dispatch stop; on a terminal event after `begin-fulfillment` the walk voids the wave-1 drafts and
cancels any activated subscription, so neither path leaves a draft for a TTL this gear does not
own.

#### Compensation Walk Order (normative)

**ID**: `cpt-cf-bss-orders-workflow-seq-compensation-walk-order`

The walk order is **strict reverse forward-execution order over compensation subjects**,
descending `owf_compensation_record.compensation_sequence`, executed inside `compensate-order`.
This is the single ordering rule; no other ordering statement in this set overrides it, and no
definition expresses it (§4.7).

`cpt-cf-bss-orders-workflow-adr-saga-compensable-no-pivot` describes the same walk as "activated
lines receive activated-cancel first, then draft lines receive draft-void". Under the two-wave
barrier (`cpt-cf-bss-orders-workflow-adr-two-wave-activation-barrier`) every wave-1 create
precedes every wave-2 activation, so for a plain order the two phrasings produce the identical
sequence. They diverge only where a create is accepted after an activation: a wave-1 rebuild, or
an override-attached subscription verified late. **In every such case reverse execution order is
authoritative**, because the property the walk preserves is dependency-safe unwind, carried by
execution order, not by wave membership. A line created and activated at `t1` and a line still
`draft` at `t2` is compensated `t2` first, then `t1`. This is also why the walk cannot be a
definition construct: a definition sees lines and waves, not the ordinal.

#### Five-Step Cancellation Fencing

**ID**: `cpt-cf-bss-orders-workflow-seq-cancellation-fencing`

**Use cases**: `cpt-cf-bss-orders-workflow-usecase-owf-cancel-with-rollback` (PRD §6.4)

**Actors**: `cpt-cf-bss-orders-workflow-actor-owf-subscriptions`

| Step | Operation | Effect | Recorded as |
|------|-----------|--------|-------------|
| 1 — stop | `run-cancellation-fence` | Phase → `compensating` (slice 05's dispatch operations refuse); slice 03's closure port cancels open gates and closes an open park; slice 08's suspension closure port closes an open suspension; on a spawned order, one `owf_receiver_revoke` row is frozen per roster receiver of every generation not yet `revoked` — `paused` and `closed` included — and a `staged` or `pausing` pause control is superseded; nothing is sent here, `compensate-order` sends the revokes (D-243, D-248, D-269) | `dispatch_stopped_at` |
| 2 — identify | `run-cancellation-fence` | Every accepted non-terminal intent and every `in_flight`/`open` dispatch registry record of the version | `in_flight_identified_at` |
| 3 — reconcile | `compensate-order` | Each identified intent reconciled to a terminal outcome through slice 05's `reconcile-intent`; a late success counts as created; an `unresolved` forward intent is withdrawn (`SUB-O12`) and, failing that, held on an `intent-unresolved` manual task — never counted as reconciled (D-212) | `reconciliation_complete_at` |
| 4 — compensate | `compensate-order` | The walk over every subject, including late successes and override-attached subscriptions | `all_compensated_at` |
| 5 — verify | `compensate-order` | Every record `succeeded`; on a spawned order also every `owf_receiver_revoke` row attested — the receiver's fence that no active or unresolved late-applicable member of its generation remains — each generation recorded `revoked`, and no generation of Lifecycle's current grant left unrecorded; `report-outcome` repeats the last two checks and settles a superseded pause control at Lifecycle once the supersede-by-revoke terminal exists (D-243, D-248, D-269) | `no_active_verified_at`, `receiver_barrier_digest` |

**Description**: The five steps are recorded strictly in order on one row and all five **MUST**
be complete before any compensated outcome is reported (§2.2). Step 3's discovery mechanism is
slice 05's reconciliation (`cpt-cf-bss-orders-workflow-component-reconciliation-sweep`), invoked
in-process on the definition's pass cadence for a live invocation, and by the sweep worker's
dead-instance unwind for a dead one (`01 §4.16`, D-105); this slice introduces no second
discovery path. Step 4's subject set
includes override-attached subscriptions; enumerating intents alone would make step 5 answer
"none remain" from an incomplete record.

**Upstream dependency answered, pending merge: `SUB-O12`.** Step 3 supersedes an accepted
in-flight intent with its **withdrawal** — never a second submit. Subscriptions PR #5131
(`fr-intent-protocol` item 2) adopts it: a request accepted but not yet applied is withdrawn by its
submitter and ends `failed` with reason `withdrawn`; where an OSS work order is already outstanding
the request terminates only after that work order is cancelled with a compensating deprovision;
an applied request cannot be withdrawn and is handled by the compensation walk instead. Step 3
therefore records a `withdrawn` outcome like any other terminal failure of the intent, and an
intent that reaches the sweep floor `unresolved` is withdrawn the same way rather than waited out
(decision D-212). Until #5131
merges, step 3 keeps its fallback — wait for the accepted intent to reach a terminal outcome, at
worst the sweep floor (30 reads or 23 h) — and never invents a second-submit workaround, which the
caller-side duplicate protocol forbids (decision D-209). The floor is not a terminal outcome
under either path: an intent still `unresolved` holds step 3 open on an `intent-unresolved` manual
task until an authoritative read or a confirmed withdrawal resolves it (D-212), so the fallback
can delay a cancellation but never lets it complete over an unreadable create.

### 3.7 Database schemas & tables

- [ ] `p3` - **ID**: `cpt-cf-bss-orders-workflow-db-compensation`

This slice keeps its two tables, loses none and gains a third, `owf_receiver_revoke` (decision
D-269): it never owned a timer, a retry-state or a dead-letter table. All three are written only
through the step envelope by the operations named in their **Ownership** line, and all three are
**deliberately mutable** — they settle in place — which is why none is on `01 §3.7`'s
append-only list; every mutation is mirrored by an audit entry
(`compensation`, `phase-transition` or `step-completion`) in the same transaction, and the audit
chain, not these rows, is the history.

#### Table: owf_compensation_record

**ID**: `cpt-cf-bss-orders-workflow-dbtable-compensation-record`

**Schema**:

| Column | Type | Description |
|--------|------|--------------|
| `id` | uuid | Primary key |
| `correlation_id` | uuid, NOT NULL | Owning process instance; FK to `owf_process_instance` |
| `resource_tenant_id` | uuid | Resource-recipient axis; NOT NULL |
| `seller_tenant_id` | uuid | Selling-party axis; NOT NULL. A compensation failure is projected into the seller-scoped operator queue (slice 07) |
| `order_id` | uuid | Order the compensation belongs to |
| `order_version` | bigint | Frozen order version the run compensates |
| `order_line_id` | text | Line item reference (join key to `FulfillmentTask`) |
| `compensation_sequence` | bigint | The subject's ordinal in **forward execution order**, frozen at step 3 completion from `owf_provisioning_intent.execution_seq` (or, for an override-attached subscription, from `owf_manual_task.override_execution_seq`, the next ordinal of the same per-order sequence, minted at verification; never a timestamp, D-268). The walk is the descending scan of this column |
| `leg` | enum | `draft-void` \| `activated-cancel` — the leg that applied, or would have applied under `already-absent`; NULL until the subject's phase is resolved |
| `subscription_ref` | text, nullable | **The target subscription**, as identified by Subscriptions: from the forward intent's confirmed identifier or from slice 07's verified override. NULL only where no subscription was ever created for the line |
| `source_intent_id` | uuid, nullable | The forward `owf_provisioning_intent` this compensates. NULL only for an override-attached subscription |
| `compensation_intent_id` | uuid, nullable | The compensating `owf_provisioning_intent` row submitted for this subject; NULL before submission and under `already-absent` |
| `idempotency_key` | text, nullable | The **compensating** intent's key (§2.1); never the forward key. NULL exactly when `resolution_mode = already-absent` or before submission |
| `resolution_mode` | enum, nullable | `compensated` \| `already-absent`; NULL until resolved |
| `outcome` | enum, nullable | `succeeded` \| `failed-pending-escalation`; NULL while unsettled. Settles in place: `failed-pending-escalation` moves to `succeeded` when a later pass completes the leg after the compensation task's `retry` mints the next `comp_attempt` (decision D-258, which dropped the override of a compensation leg). (`blocked-upstream` and its `blocked_by_record_id` column are withdrawn with the dependency topology, decision D-196) |
| `failure_reason` | enum, nullable | `draft-void-failed` \| `activated-cancel-failed`; set only with `outcome = failed-pending-escalation` |
| `submission_failure_count` | integer, NOT NULL | Submissions of this leg that Subscriptions did not accept, and passes that found its compensating intent settled terminal `failed` (`inst-co-failed-leg`, D-258); bound 5 (§3.2); starts at 0, and again at each new `comp_attempt` |
| `comp_attempt` | integer, NOT NULL, DEFAULT 1 | The leg's compensating-key attempt; incremented only by this slice's compensation-record port `next_comp_attempt(recordRef)`, which slice 07's compensation-task `retry` calls in its unit of work (`07 §3.6` `inst-rmt-retry`) and which, for a leg whose compensating intent is settled `failed`, also resets `submission_failure_count` and clears the recorded intent so the next pass submits under the new key; a leg whose intent is still live is left unchanged (D-258) |
| `at_sale_facts_emitted` | boolean | Whether at-sale billable facts had been posted for this line before compensation. **Derived locally, never read from Billing**: `true` exactly when this line's wave-2 activation intent reached a confirmed `activated` outcome before the fence was claimed |
| `manual_task_id` | uuid, nullable | The slice-07 `ManualTask` raised for this record's escalation; set with `outcome = failed-pending-escalation`. Never an incident |
| `attempt_id` | text, nullable | The platform attempt identifier of the `compensate-order` pass that last settled this record (`01 §3.3` *Attempt identity*); the join to the platform timeline |
| `created_at` / `resolved_at` | timestamp | Freeze and settlement times |

**PK**: `id`

**Constraints**: `(order_id, order_version, order_line_id, source_intent_id)` unique — one
subject per forward intent per order version; `UNIQUE (idempotency_key)` where non-null, over the
whole table (not partitioned, `01 §3.7`, D-104);
`UNIQUE (order_id, order_version, compensation_sequence)` so the walk order is total and
gap-free; `failure_reason` NOT NULL exactly when `outcome = failed-pending-escalation`.

**Ownership**: written only by `compensate-order`
(`cpt-cf-bss-orders-workflow-component-compensation-executor`) through the step envelope.

**Mutability**: deliberately mutable (resolution, outcome, counters, `attempt_id`); no DELETE
grant to any role except the retention worker's bounded row-wise purge.

**Additional info**: Tenant axes `resource_tenant_id` (always) and `seller_tenant_id` (operator
queue). Indexed on `(order_id, order_version, compensation_sequence DESC)` for the ordered scan and
on `(seller_tenant_id, failure_reason)` for the manual-task queue. **Retention ≥ 400 days**,
matching the audit and manual-task stores: a compensation record is the evidence behind a
`fulfillment_failed` or `cancelled` acknowledgement. Growth table, **not partitioned** (`01 §3.7`,
D-104) — purged row-wise through a `created_at` index; the `retention-purge` worker of `01 §3.8` gains this table and
`owf_cancellation_fence` at their windows (the retention-purge roster in `01 §3.8` covers both).

**Example**:

| compensation_sequence | leg | resolution_mode | outcome | failure_reason | at_sale_facts_emitted |
|-----|-----|-----|---------|-----------------|------------------------|
| `4` | `activated-cancel` | `compensated` | `failed-pending-escalation` | `activated-cancel-failed` | `true` |
| `3` | `activated-cancel` | `compensated` | `succeeded` | (null) | `true` |
| `2` | `draft-void` | `already-absent` | `succeeded` | (null) | `false` |
| `1` | `draft-void` | `compensated` | `succeeded` | (null) | `false` |

#### Table: owf_cancellation_fence

**ID**: `cpt-cf-bss-orders-workflow-dbtable-cancellation-fence`

**Schema**:

| Column | Type | Description |
|--------|------|--------------|
| `order_id` | uuid | Order under fencing |
| `order_version` | bigint | Frozen order version |
| `correlation_id` | uuid, NOT NULL | Owning process instance |
| `resource_tenant_id` | uuid | Resource-recipient axis; NOT NULL |
| `seller_tenant_id` | uuid | Selling-party axis; NOT NULL |
| `trigger` | enum | `failure` \| `cancel` \| `supersede` \| `terminal-event` — the trigger that claimed the run, after any promotion; decides `report-outcome`'s mode |
| `promoted_from` | enum, nullable | `failure` where a cancellation promoted the run (§4.3); NULL otherwise |
| `orders_failure_reason` | enum, nullable | The Orders catalogue reason (`01 §4.9`) of the cause `inst-fence-cause` found on `failure`; kept on promotion. The exact reason on the Orders side, carried in `OrderFulfillmentAborted`'s `data` |
| `failure_reason` | enum, nullable | Lifecycle's closed `failure_reason` value, mapped from `orders_failure_reason` by §4.8, set on `failure`; carried by `report-outcome` to `acknowledge-failed` |
| `cancel_request_ref` | uuid, nullable | Slice 09's cancel request record (`owf_cancel_request`), set on `cancel` or promotion; its `cancel_reason` is the reason `workflow-cancel` carries |
| `trigger_event_id` | uuid, nullable | The Lifecycle event of a `supersede` or `terminal-event` run |
| `claimed_by_attempt_id` | text, NOT NULL | Platform attempt identifier of the claiming `run-cancellation-fence` call |
| `dispatch_stopped_at` | timestamp | Step 1 completion |
| `in_flight_identified_at` | timestamp | Step 2 completion |
| `in_flight_count` | integer | Intents and registry records step 2 identified |
| `reconciliation_complete_at` | timestamp, nullable | Step 3 completion; never set while a forward intent of the version is `submitted` or `unresolved` (D-212) |
| `all_compensated_at` | timestamp, nullable | Step 4 completion — every subject has a settled outcome |
| `no_active_verified_at` | timestamp, nullable | Step 5 completion — every record `succeeded` |
| `receiver_barrier_digest` | text, nullable | Step 5, spawned orders only: the digest of the attestations on the version's `owf_receiver_revoke` rows, from which `report-outcome` builds the typed barrier evidence (Lifecycle D-198; D-243, D-269); NULL where no grant was recorded. NOT NULL with `no_active_verified_at` whenever the version has a grant. Re-derived by a later step 5 when a generation recorded late adds rows (`inst-co-step5`); a frozen generation with no rows (an empty roster) contributes the digest over the empty set (D-298) |
| `reported_at` / `reported_outcome` | timestamp / enum, nullable | Set by `report-outcome` when the outcome is recorded |
| `absorbed_trigger_count` | integer | Later triggers absorbed against this run (§4.3); starts at 0 |
| `last_pass` | integer, nullable | The highest `compensate-order` pass presented for the run, written by its pass guard (`inst-co-fence-guard`) in the pass's settlement unit of work, so a pass whose replica died after its lease committed may have left it unwritten; NULL until the first pass settles one. A pass above it is admitted, so the definition's `pass + 1` after a dead pass is accepted, and the dead-instance unwind presents max(`coalesce(last_pass, -1)`, the highest pass of the instance's `compensate-order` key family in the registry) + 1 — 0 when neither exists — which is above both (`01 §4.16` item 2, decisions D-247, D-270) |
| `reauthorization_required_at` | timestamp, nullable | Set by `compensate-order` (`pre-compensation`) when slice 08's cancel-authority port answers `withdrawn` on a `cancel` run; while set, no further withdrawal or leg is submitted (D-268). Cleared only by `run-cancellation-fence` when a newly authorized cancel is absorbed against the run and replaces `cancel_request_ref` (§4.3, `inst-fence-claim`) — the fence's one rewritable stamp (D-182) (decision D-84: the fence carries the awaiting-re-authorization mark that `08 §4.3` assigns to slice 06) |
| `created_at` | timestamp | When the run was claimed |

**PK**: `(order_id, order_version)`

**Constraints**: steps recorded strictly in order — a later step's column is NOT NULL only once
the preceding one is set; `orders_failure_reason` and `failure_reason` NOT NULL when `trigger = failure` or `promoted_from = failure`;
`cancel_request_ref` NOT NULL when `trigger = cancel`. The PK is the mutual-exclusion token: the
insert that claims a run wins, and a losing insert absorbs rather than retries.

**Ownership**: `run-cancellation-fence` inserts the row and writes steps 1–2, the trigger and its
promotion, and, when it absorbs a newly authorized cancel against a `cancel` run whose
`reauthorization_required_at` is set, replaces `cancel_request_ref` with the new request and
clears the mark in that absorption's transaction (§4.3, decision D-182); `compensate-order`
writes steps 3–5, `receiver_barrier_digest` and `last_pass`, and sets `reauthorization_required_at`; `report-outcome` writes `reported_at`
and `reported_outcome`. No other writer.

**Mutability**: deliberately mutable (step stamps, promotion, counter, report stamps); a stamp once
set is never cleared, with one stated exception: the re-authorization mark
`reauthorization_required_at` is the fence's one rewritable stamp; `receiver_barrier_digest` is a
derived value, not a stamp, re-derived when step 5 records a late generation (D-269). `compensate-order` sets it on a
`withdrawn` re-check, and only `run-cancellation-fence`'s absorption of a newly authorized cancel
clears it, replacing `cancel_request_ref` in the same write; a later `withdrawn` re-check may set
it again. `cancel_request_ref` changes only in that absorption, once set (D-182).

**Additional info**: Tenant axes `resource_tenant_id` and `seller_tenant_id` (operator
visibility). Read by `report-outcome` as the gate; a missing row is itself a refusal. **Retention
≥ 400 days**. Growth table, **not partitioned** (`01 §3.7`, D-104) — purged row-wise through a
`created_at` index.

#### Table: owf_receiver_revoke

**ID**: `cpt-cf-bss-orders-workflow-dbtable-receiver-revoke`

One row per receiver of every generation an unwind revokes (decision D-269): the send ledger and
the write-once evidence `compensate-order`'s step 5 and `report-outcome` read.

**Schema**:

| Column | Type | Description |
|--------|------|--------------|
| `order_id` | uuid | Order under fencing |
| `order_version` | bigint | Frozen order version |
| `generation` | integer | The receiver generation revoked — one of the version's `owf_fulfillment_grant` rows (`05 §3.7`) |
| `grant_id` | uuid | That generation's grant |
| `receiver_ref` | text | A receiver of the generation's roster (`05 §3.7` `owf_provisioning_intent.receiver_ref`) |
| `correlation_id` | uuid, NOT NULL | Owning process instance |
| `resource_tenant_id` | uuid | Isolation column; NOT NULL |
| `revoke_key` | text, NOT NULL | `{tenant}:{orderId}:{orderVersion}:receiver-revoke:{generation}:{receiverRef}`, the receiver command's key; every send reuses it |
| `send_count` | integer | Revokes sent under the key (`inst-co-revoke`); starts at 0 |
| `last_sent_at` | timestamp, nullable | Database time of the last send |
| `attestation` | jsonb, nullable | Write-once: the receiver's revoke attestation — `fence_id`, `revoked_generation`, `revoked_at`, each roster target's outcome and `complete` — written only when the receiver marks it complete; the per-receiver entry of the barrier evidence and of Lifecycle's `ControlRevokeEvidence` |
| `attested_at` | timestamp, nullable | Write-once; set with `attestation` |
| `created_at` | timestamp | When the row was frozen; the retention index's column |

**PK**: `(order_id, order_version, generation, receiver_ref)` — the uniqueness that makes the
freeze idempotent: a replayed fence or step 5 inserts nothing new.

**Constraints**: `attested_at` NOT NULL exactly when `attestation` is; `attestation.revoked_generation`
≥ `generation`.

**Ownership**: inserted by `run-cancellation-fence` in fencing step 1 (`inst-fence-step1`) and by
`compensate-order`'s step 5 for a generation of Lifecycle's current grant recorded late
(`inst-co-step5`); `send_count`, `last_sent_at`, `attestation` and `attested_at` written by
`compensate-order` (`inst-co-revoke`); read by `compensate-order`'s step 5 and by `report-outcome`
(`inst-ro-receivers`, `inst-ro-call`). No other writer.

**Mutability**: deliberately mutable in `send_count` and `last_sent_at`; `attestation` and
`attested_at` are written once and never changed.

**Additional info**: Tenant axis `resource_tenant_id`. Indexed on `(order_id, order_version) WHERE
attested_at IS NULL` for the pass's send and on `(created_at)`, the retention index. **Retention ≥
400 days**, as `owf_fulfillment_grant`: the rows are the receiver evidence behind a
`fulfillment_failed` or `cancelled` acknowledgement; purged row-wise through the `created_at`
index, never a row whose instance is not terminal (`01 §3.8` *Retention purge*). Not partitioned
(`01 §3.7`, D-104).

### 3.8 Deployment Topology

- [ ] `p3` - **ID**: `cpt-cf-bss-orders-workflow-topology-compensation`

No deployment topology beyond `01 §3.8`: the three operations are routes of the Orders Workflow
service, and this slice adds no worker. It does not add to the three-worker roster
(`reconciliation-sweep`, `retention-purge`, `audit/<tenant>`); the sweep worker's instance
liveness pass raises an unwind whose invocation died as an `invocation-dead` task and, after an
operator's cancel, re-invokes `compensate-order` in-process by pass (`01 §4.16`, D-105); the
definition is what re-invokes it for a live one.

## 4. Additional context

### 4.1 Run-time guards the operations enforce

Independently of the definition's fence (ADR-0012 *Run-time guards*):

1. [ ] - `p1` - `compensate-order` **MUST** refuse `fence-not-claimed` without an `owf_cancellation_fence` row for the version, and **MUST NOT** freeze subjects before `reconciliation_complete_at` is set, which it **MUST NOT** stamp while a forward intent of the version is `submitted` or `unresolved` (D-212) - `inst-g06-fence-first`
2. [ ] - `p1` - `report-outcome` **MUST** refuse `outcome-not-reportable` for `failed`, `cancelled`, `superseded` or `terminal-event` while `no_active_verified_at` is null, and for `completed` while slice 04's completion predicate does not hold or a fence row exists - `inst-g06-report-gate`
3. [ ] - `p1` - `report-outcome` **MUST** be the only caller of `fulfillment-acknowledgement` and `workflow-cancel` in this gear, and **MUST** take its mode from the fence row's trigger, never from the input alone - `inst-g06-sole-reporter`
4. [ ] - `p1` - On `superseded` or `terminal-event`, `report-outcome` **MUST** make no Lifecycle call - `inst-g06-no-call-modes`
5. [ ] - `p1` - A compensation-leg failure **MUST** create a manual task through slice 07's creation port under either partial-failure policy, and **MUST NOT** create an incident - `inst-g06-leg-task`
6. [ ] - `p1` - Every compensating intent **MUST** carry the compensating key of §2.1; a settled answer whose fingerprint does not match **MUST** be treated as `idempotency-key-conflict` - `inst-g06-distinct-key`
7. [ ] - `p1` - `report-outcome` **MUST NOT** call `acknowledge-failed` or `cancel-workflow-mediated` for a version whose `begin-fulfillment` has not committed, and **MUST** answer every Lifecycle outcome by §4.9 - `inst-g06-from-fulfillment`

### 4.2 The unilateral-cancel window and no Billing wait

**FulfillmentTask unilateral-cancel window.** A `FulfillmentTask` is unilaterally cancellable
until its **activation** intent is accepted by Subscriptions; draft-create acceptance does not
close the window, because a `draft` subscription is not resource-affecting and carries no billable
facts. From activation-intent acceptance onward the task **MUST** be reconciled to a terminal
outcome (fence step 3) before the order-level outcome is reported.

**No Billing wait.** `report-outcome` reports as soon as `compensate-order` answers `complete`.
This gear only *records* whether at-sale facts had been emitted, as evidence; it never triggers or
awaits their reversal.

**Distinct manual-task reasons.** `draft-void-failed` and `activated-cancel-failed` are never
merged on the manual-task queue: a stuck draft-void points the operator at a still-`draft`
subscription; a stuck activated-cancel points at an active subscription still billing and
consuming resources, a materially more urgent state.

### 4.3 Concurrent and late triggers (normative)

**One key per trigger request.** The fence's step key ends in the reference of the request that
triggered it: the cancel request, or the supersede or terminal event. The request body carries the
same reference, and the registry fingerprints the body (`01 §3.7`, `request_fingerprint`), so a key
that named only the trigger kind would turn every second request of that kind into an
`idempotency-key-conflict`. That refusal is a permanent failure the definition does not catch
(`10 §3.6` fragment (c), the `fence` task), so it would fault the invocation partway through
compensation. With the reference in the key:

- a **replay** of the same request (a platform retry, or a re-run after a crash) derives the same
  key and fingerprint and is an absorbed duplicate of the registry;
- a **different** request of the same kind (a second authorized cancel) is a first call under its
  own key, which runs the operation and reaches the losing insert below. A supersede or a
  terminal event arrives at most once per order version, so its `triggerEventId` changes nothing
  today; it keeps the rule uniform, so a trigger kind added later cannot reintroduce the collision;
- a `failure` run carries no request reference. The definition's three tasks that set `unwind: failure`
  (`planFailFastUnwind` and `preActivationAbort` in `10 §3.6` fragment (b), and `toFailureUnwind`,
  reached through the failure stage's `failFastUnwind` switch, in fragment (c)) all leave for the
  unwind stage, and after the fence the phase is `compensating`, so a version gets
  at most one `failure` fence call. A failed compensation leg is `compensate-order`'s, not a new
  fence call. A replay therefore presents the same body — the failure reason is resolved from the
  record (§3.6 `inst-fence-cause`, §4.8), never presented — and is absorbed.

The key never includes a platform-supplied value (ADR-0006 as amended). Decision D-74 records the
component.

The `owf_cancellation_fence` primary key is the arbiter; `run-cancellation-fence` applies this
table on a losing insert:

| Situation | Rule |
|-----------|------|
| `cancel` arrives while a `failure` run is walking | Absorbed. The walk, subjects and steps are unchanged. The row's `trigger` is **promoted** to `cancel`, `promoted_from = failure`, `cancel_request_ref` set, and `report-outcome` consequently submits the workflow-mediated cancel instead of `acknowledge-failed`. `absorbed_trigger_count` incremented |
| A second `cancel` arrives while a `cancel` run is walking | Absorbed, no change beyond `absorbed_trigger_count` — unless `reauthorization_required_at` is set, in which case the newly authorized request replaces `cancel_request_ref` and the mark is cleared, so the walk resumes under the new authority. The second authorization stays in the audit trail with its own actor (slice 08) |
| `cancel` arrives after `no_active_verified_at` but before `report-outcome` settles | Absorbed and promoted as in row 1 when the run is `failure`; the outstanding report then goes out as `cancelled` under its own lifecycle-transition key |
| `cancel` arrives while a `supersede` or `terminal-event` run is walking | Absorbed without promotion: the row keeps its trigger and answers it as `effectiveTrigger`, and the definition takes `reportAs` and `terminationKind` from that answer, never from the path it entered by (`10 §3.6` (c)), so the report stays the supersede or terminal-event one. `absorbed_trigger_count` incremented |
| Any trigger arrives after `report-outcome` settled (`reported_at` set on the row) | Not absorbed: the fence refuses `outcome-already-reported` before the claim (`inst-fence-cause`), as `authorize-cancel` does for a cancel, so no second report or `OrderFulfillmentAborted` follows; a dead invocation in that state is terminated by the liveness pass (`01 §3.8` item 2, decision D-271) |
| `supersede` or `terminal-event` arrives while any run is walking | Absorbed without promotion; `terminate-on-terminal-event` already answers `terminate = false` on a `compensating` instance (slice 02), and the claimed run's own path reaches `terminate-instance` |
| `OrderAmended` races an in-flight run | The amendment creates a **new order version**; the run stays frozen against its `(order_id, order_version)`, completes and reports against it; the new version starts its own instance per slice 02's unwind-then-start rule |

The run's subject set and walk order are immutable once frozen. Only the terminal *report* may
change, and only in the one direction where the stronger authorization (a cancellation)
supersedes the weaker (a failure determination).

### 4.4 Compensation cannot exceed the step's declared action

If activated-cancel cannot complete, the Workflow does not attempt a further undo of what
activation caused inside OSS — it stops at activated-cancel, escalates via manual task, and
leaves the order non-terminal. The same applies to a stuck draft-void.

### 4.5 The unilateral window is fence-visible

The window rule of §4.2 is enforced by fence step 2: an intent that was accepted for activation is
in the identified set and cannot be skipped by step 3; a task holding only a draft is reached by
the walk's single draft-void leg.

### 4.6 Upstream dependency answered: `SUB-O1`

The activated-cancel leg needs a cancellation reason that is neither an early-termination reason
nor a plan-change supersession reason — reusing either would misattribute a termination fee or
credit, or misrepresent the compensation as a customer-driven plan change. The canonical
Subscriptions register carries this ask as `SUB-O1`, marked **critical**. Subscriptions PR #5131
(`fr-cancel-reasons`, still open) answers it: the closed, versioned reason set gains
**`order_compensation`**, used only by the order path to roll back a subscription activated by an
order whose fulfillment then failed, and outside the early-termination class, so it derives
neither an early-termination fee nor an unused-portion credit. The activated-cancel leg sends that
value. A one-time fact already emitted at activation stays posted; its reversal is a Billing-chain
artifact the compensation cancel triggers, which this gear never waits on (§2.1). Before activation
the leg is the draft void, which needs no reason. Until #5131 merges, the value is agreed but not
yet canonical (decision D-209).

### 4.7 Constraints this slice places on the definition

These are inputs to the validation rules of ADR-0012 and `10 §2.2`.
[`10 §4.7`](./10-process-definition.md#47-what-a-definition-change-may-and-may-not-do) *Slice constraints* maps each item below: an
enforced item is refused through the rule or fence row it restates; every other item is
canonical-definition guidance, which the canonical version carries and the behavioural gate of
`10 §4.2` asserts for every candidate version (decision D-136).

1. [ ] - `p1` - **Unwind order.** On every failure, cancel, supersede and terminal-event path: `run-cancellation-fence` **<** `compensate-order` **<** `report-outcome` **<** `terminate-instance`, with the `trigger` of `run-cancellation-fence` matching the path (`failure`, `cancel`, `supersede`, `terminal-event`) and the `outcome` of `report-outcome` derived from the fence's `effectiveTrigger` (`failed`, `cancelled`, `superseded`, `terminal-event`), never from the path's entry, because an absorbed trigger keeps the claimed run's report (§4.3) - `inst-def06-unwind-order`
2. [ ] - `p1` - **Loop until complete.** Only `compensationState = complete` may reach `report-outcome`. `in-progress` and `pending-escalation` **MUST** return to `compensate-order` with `pass` incremented, after a `wait` or a manual-task resolution `listen`, and **MUST NOT** reach `report-outcome` or `terminate-instance`. Fragment (c) routes both through `awaitCompensationResolution`, whose 1 h `retryLeg` is inside the sweep floor; a dedicated `in-progress` case re-entering `compensate` on the barrier's poll interval is the recommended refinement for the fragment's owner. A `report-outcome` answering `lifecycleCall = revoke-pending` **MUST** likewise return to `compensate-order` under the next `pass` and reach `report-outcome` again under its `nextRound` (§3.6 `inst-ro-receivers`, decision D-269) - `inst-def06-loop`
3. [ ] - `p1` - **Pass numbering.** Every re-invocation of `compensate-order` **MUST** present a `pass` greater than the last one presented for the instance, and a platform retry of one attempt **MUST** present the same `pass`; `pass` is kept in `$context` and is the only walk state the definition holds - `inst-def06-pass`
4. [ ] - `p1` - **No per-line walk.** No definition **MAY** submit, order or skip an individual compensating leg; there is no operation for one, and the walk order is not expressible from the definition's data (§3.6 *Compensation Walk Order*) - `inst-def06-no-line-walk`
5. [ ] - `p1` - **No swallowing.** None of the three operations **MAY** sit in a `try` whose `catch` continues the forward path. Retry exhaustion of `compensate-order` **MUST** route to the unwind's own resolution arm (`awaitCompensationResolution`) and re-enter with the next `pass`; a spent retry budget or a permanent failure of `run-cancellation-fence` or `report-outcome` **MUST NOT** be caught: it faults the invocation, which the instance liveness pass raises as the `invocation-dead` task (§4.9, `10 §4.6`, decision D-114). Every permanent failure of either is a defect of the definition or of a contract with Lifecycle — a missing cause, a mismatched mode, evidence Lifecycle refuses — that no retry of the same step resolves, and the unwind has already claimed the fence, so no stage checkpoint is left to re-enter; the unwind's `report-outcome` carries the family's round like every other call of it - `inst-def06-no-swallow`
6. [ ] - `p1` - **Completion only after the last wave.** `report-outcome` with `outcome: completed` **MUST** follow `dispatch-wave2-activate` and **MUST NOT** follow `run-cancellation-fence` on the same path (`10 §4.1`) - `inst-def06-completion`
7. [ ] - `p1` - **Signals during the unwind.** The competing `fork` of the unwind's resolution arm **MUST** contain the `cancel-requested` arm, routed through `authorize-cancel` to `run-cancellation-fence` (which absorbs and promotes, §4.3) and back to `compensate`. The unwind **MUST NOT** contain a hold arm — the phase table of `01 §3.7` has no `compensating → suspended` transition, and a hold does not pause compensation - `inst-def06-signals`
8. [ ] - `p2` - **Listens.** This slice requires no `listen` of its own: compensating-intent confirmations reach Orders' consumer and `reconcile-intent`, and `compensate-order` reads them from the record on its next pass - `inst-def06-no-listen`

### 4.8 The failure reason is resolved from the record and mapped (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-constraint-failure-reason-mapping`

`run-cancellation-fence` on `failure` **MUST NOT** take a failure reason from the definition. The
cause step 2 found (`inst-fence-cause`) names the Orders catalogue reason, and the fence records
it as `orders_failure_reason` together with the one value of Lifecycle's closed `failure_reason`
enumeration the table below maps it to
([Lifecycle `06 §4.4`](../../../orders/docs/features/06-workflow-seam.md#contract-06-4-4),
Lifecycle D-136). Lifecycle refuses any other value `request-invalid` at its boundary, and a
missing one `failure-reason-missing`. The per-subject `reason` in the definition's `$context.failureSubjects`
is fragment (c)'s manual-task reason and never reaches the fence (decisions D-110, D-116).

| Cause the fence found | `orders_failure_reason` | Lifecycle `failure_reason` |
|-----------------------|-------------------------|----------------------------|
| A forward line task resolved `exhausted`, or under `fail-fast` a forward line in `failed` — never a line whose activation was refused `refusalKind = unresolved` (the unresolved row below) | the line's reason — `wave1-create-failed`, `wave2-activation-failed`, `never-dispatched` or `intent-unresolved`; for several lines, the line with the lowest `execution_seq` | `line-execution-failed` |
| The same, for a wave-2 line whose activation Subscriptions refused as an overlap collision (`refusalKind = overlap` on the intent row and in `failed[]`, `05 §3.7`, decision D-305) | `wave2-activation-failed` | `overlap-collision` (a collision raised after the re-check, [Lifecycle `06 §4.3`](../../../orders/docs/features/06-workflow-seam.md#contract-06-4-3)). Inert until `SUB-O11` gives slice 05 the typed refusal code that records `overlap` ([`../UPSTREAM_REQS.md`](../UPSTREAM_REQS.md) §2.1); until then such a refusal arrives without a typed code and takes the unresolved row below (D-285) |
| A plan task resolved `exhausted` for a plan that could not freeze because the expected-fulfillment instant is at or after the version's accepted-binding deadline (`04 §3.6`, `planState = binding-expired`) | `order-binding-expired` | `order-binding-expired` (decision D-194; Lifecycle `06 §4.4` lists it) |
| A settled `re-check-pre-activation` abort on an elapsed accepted-binding deadline (`04 §3.6`) | `order-binding-expired` | `order-binding-expired` |
| The wave-2 guard's `bindingExpired: true` answer (`05 §3.6` `inst-pi-wave2-guard`) or the SLA check's `binding-expired` answer on an open unresolved-refusal task — the plan's `activation_deadline_at` elapsed (`07 §4.2`, D-294) — or a wave-2 line Subscriptions refused with a **typed verified expiry, explicit closure or temporary end** of the accepted binding (`refusalKind = verified-expiry` on the intent row and in `failed[]`, `05 §3.3`; Lifecycle `06-workflow-seam.md` *D-201 receiver error mapping*, decision D-257; the refusal half is inert until `SUB-O11` gives slice 05 the typed refusal code ([`../UPSTREAM_REQS.md`](../UPSTREAM_REQS.md) §2.1, D-279), and until then such a refusal arrives without a typed code and takes the unresolved row below, D-285) — in each case after any line may already be `activated`, so the fence voids the drafts and cancels the activated lines first and reports only with `no_active_subscription_remains = true`; never a pre-activation abort once a line is active | `order-binding-expired` (the line's own `wave2-activation-failed` stays on the task) | `order-binding-expired` |
| A wave-2 line whose activation Subscriptions refused as a market divergence — the payer's commercial profile at activation no longer matches the currency and region the line was priced in (Subscriptions PR #5131 `fr-order-originated-create` item 6; `refusalKind = market`) | `wave2-activation-failed` | `market-divergence` (the same reason as the re-check's own abort, raised by the receiver after it; decisions D-209, D-257) |
| A wave-2 line Subscriptions refused with a typed code on any other named binding ground — a digest or identity mismatch, retirement alone, corrupt evidence or lost SKU protection (no `refusalKind`): an ordinary line failure, remediated under `remediate` and reaching the fence only once its task is `exhausted` (or under `fail-fast`), i.e. once remediation conclusively fails; none of these supplies expiry proof, and no generic refusal is read as expiry (Lifecycle *D-201 receiver error mapping*, decision D-257); inert with the typed codes until `SUB-O11` lands them (D-285) | `wave2-activation-failed` | `line-execution-failed`, only after ordinary compensation is established |
| A wave-2 line Subscriptions refused with an unknown code, a denial or an outage — no code of the rows above, which until `SUB-O11`'s typed codes land is every refusal but a market divergence (`refusalKind = unresolved`, `05 §3.7`) — **never a cause**: Lifecycle's mapping keeps unknown codes, denial and outage unresolved and admits no generic upstream-error adapter. The line's task is opened under either policy, is never exhausted and offers no task `cancel` (`07 §4.2`, §4.4); the line leaves only by a retry or a verified override that lands it, by the order cancel, whose fence takes the `cancel` cause (D-285), by the failure unwind of another line — its typed failure under `fail-fast`, which ends the order at once (D-300), its remediation exhausted under `remediate`, or its external activation or verified expiry under either policy (D-257, D-330) — whose cause is that line's, or, once the plan's `activation_deadline_at` has elapsed, by the failure unwind on that deadline, the `order-binding-expired` row above (D-294) | — (none) | — none: no acknowledgement is sent on it |
| A settled `re-check-pre-activation` abort | `overlap-collision` | `overlap-collision` |
| A `draft_create` intent whose subscription the re-read found active without an activation of this gear (`05 §4.4`, D-233) | `draft-activated-externally` | `line-execution-failed`, interim |
| A settled `replace-fulfillment-grant` answering `refused` (`05 §3.3`, D-243) | `grant-replacement-refused` | `line-execution-failed`, interim |
| An order-scope `receiver-control-abandoned` task resolved `exhausted` or by `cancel` (`08 §3.6`, D-243) — reached by no route since D-273: an order-scope task is never `exhausted` (`07 §4.2`) and this one offers no task `cancel`; the order cancel unwinds on the `cancel` trigger. The row keeps the catalogue reason mapped | `receiver-control-abandoned` | `line-execution-failed`, interim |
| — | `market-divergence` | `market-divergence` |
| — the occupancy port's defer ladder exhausted | `overlap-read-unevaluable` | `overlap-presence-unevaluable` |
| — the identity port's defer ladder exhausted | `identity-party-unavailable` | `identity-party-unavailable` |
| — the authorization aged past its validity, the readiness port unreadable at the re-check (`04 §3.6` `inst-rc-if-stale`, D-250) | `payment-authorization-stale` | `line-execution-failed`, interim |
| — the re-check's readiness re-read answered a new refusal (`04 §3.6` `inst-rc-if-stale`, D-226, D-250) | `financial-readiness-withdrawn` | `line-execution-failed`, interim |

The six interim rows have no Lifecycle value. Until
`cpt-cf-bss-orders-workflow-upreq-lifecycle-failure-reason-coverage` adds one for each
([`../UPSTREAM_REQS.md`](../UPSTREAM_REQS.md) §2.4), the fence sends the interim value and keeps
the exact reason in `orders_failure_reason` and in `OrderFulfillmentAborted`'s `data` (`01 §4.7`).
Only Lifecycle's audit `caller_reason` and `OrderFulfillmentFailed` are approximate, and this is
the residual the ask closes.

**Lifecycle D-172 (2026-10-02).** Lifecycle withdrew `dependency-graph-invalid` as an emitted value
(it stays in its closed set for replayed payloads), confirmed `order-binding-expired`, and keeps
`line-execution-failed` as the interim for a stale authorization; the table above already emits no
dependency-graph value since D-196. Lifecycle's receipt floor for these triggers is 30 days (D-173),
so a fence re-run after day one replays the stored outcome rather than refusing `not-admissible`.

### 4.9 Every Lifecycle answer to `report-outcome` (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-constraint-report-outcome-answers`

`report-outcome` **MUST** answer each outcome of `fulfillment-acknowledgement` and `workflow-cancel`
([Lifecycle `06 §3.3`, `§3.6`](../../../orders/docs/features/06-workflow-seam.md#contract-06-3-6);
refusal registry [Lifecycle `01 §4.7`](../../../orders/docs/DESIGN.md#contract-01-refusal-reasons-are-derived-gts-error-types))
as below. A permanent failure is not caught by the definition and faults the invocation
(`10 §3.6` (c)), which the liveness pass raises as an `invocation-dead` task (`01 §4.16`), so
no row is left to a builder's choice (decision D-111). The statuses in the rows (503, 409) name how
Lifecycle's REST surface renders each answer; in process the answer is the typed `OrdersError`, and
the rows match its variant and reason token, with any variant not listed taking the *a 5xx* row
(decision D-231). The composite-gate reasons (`payer-relationship-unconfirmed`,
`billing-terms-unsupported`, and `order-in-flight-for-key` with an `assessment` report) cannot
answer either call; one that did would take the *a 5xx* row, and so would a diagnostics payload
that fails to decode (decision D-237). Such a payload now includes a report outcome that states a
catalog scope key without a catalog selection, which the Lifecycle SDK refuses to decode (Lifecycle
D-244 item 34; decision D-241).

| Lifecycle answer | When it can arise | `report-outcome` answers |
|------------------|-------------------|--------------------------|
| A committed `completed`, `fulfillment_failed` or `cancelled`, or the stored outcome replayed under the same key | The report landed | Step 7 |
| `not-admissible`, the order read shows this report's transition applied: at `orderVersion`, `completed` with every line `activated` under the subscription identifier the report carries (`acknowledge-completed`), or `fulfillment_failed` (`acknowledge-failed`) | A re-run of a report that committed, after Lifecycle's ≥ 30-day workflow-class receipt floor (D-203) forgot the key ([Lifecycle `01-foundation.md:238-241`](../../../orders/docs/DESIGN.md#contract-01-the-idempotency-window-is-24-hours-and-is-not-a-commercial-bound)): the version check passes and the state table has no row from the state reached. Only this gear's rows 13, 14 and 26 reach those states. `cancelled` is not such evidence — an ordinary cancel (row 15) reaches it too, and the read does not show which — so a `cancel-workflow-mediated` re-run takes the terminal row below | Step 7 with `reportedOutcome` = the mode, `lifecycleCall = already-applied` (decision D-188) |
| Unavailable (503), a 5xx, a timeout; `still-processing` (409); `authorization-context-changed` (409) | Transient, a concurrent attempt of the same key, or a changed authorization context that a retry with fresh authorization resolves | `retryable-failure`, key left open |
| `not-admissible`, the order read shows `on_hold` | `acknowledge-completed` has no `on_hold` row (Lifecycle rows 13, 26) | `completed`: `lifecycleCall = held` with `nextRound`. It cannot arise for `failed` or `cancelled`, which rows 26 and 27 admit from a held order |
| `not-admissible`, the order read shows `fulfillment_failed` with `failure_reason = operator-forced-unreconciled` | Two fulfillment operators forced the overdue post-spawn order (Lifecycle D-182, rows 28 and 29). Since D-182, after the spawn signal Lifecycle leaves `in_fulfillment`, and an `on_hold` whose pre-hold state is `in_fulfillment`, by this gear's own reports **or** by that forced failure | `failed`, `cancelled`: step 7 with `reportedOutcome = terminal-event`, `lifecycleCall = none`; the walk this report closes has already compensated, and a compensation it could not complete keeps its `activated-cancel-failed` task open. `completed`: `lifecycleCall = forced-terminal`, settled with no Lifecycle call; that settled answer is the cause the fence accepts for a `terminal-event` unwind with no trigger event (§3.6 step 2), so the definition takes the terminal unwind, which cancels the activated subscriptions through Subscriptions (`10 §3.6` (b), decision D-208) |
| `not-admissible`, the order read shows any other terminal state, other than the applied transition above | The order ended by another path first — an ordinary cancel before the spawn signal (row 15) — while the unwind, which has no lifecycle arm, was walking. It cannot arise for `completed`: the definition reports completion only after `report-spawn-signal` was recorded (`10 §3.6` (b)), and once `spawn_signal_at` is set the only exits from `in_fulfillment` other than this gear's reports are the forced failure of the row above (Lifecycle rows 13, 14, 16, 26, 27, 28, 29): row 15 requires no spawn signal, row 23 applies the pre-hold state's own cancel guard, and row 24 and the missing `in_fulfillment → expired` row exclude expiry | `failed`, `cancelled`: step 7 with `reportedOutcome = terminal-event`, `lifecycleCall = none`; the walk has already removed what the version created. `completed`: `permanent-failure` `version-mismatch`, as the next row (decision D-166) |
| `not-admissible` otherwise; `prehold-not-in-fulfillment`; `version-conflict` | The order is not in fulfillment at this version, which step 4 and D-109 exclude | `permanent-failure` `version-mismatch`, the Lifecycle reason in `owf_step_log.result` |
| `compensation-evidence-missing`, `compensation-evidence-incomplete`, `acknowledgement-lines-incomplete`, `acknowledgement-subscription-missing`, `acknowledgement-subscription-duplicated` | The evidence or the roster Orders built does not satisfy Lifecycle's guard; the completion predicate's distinct-identifier clause (§3.2) is the Orders-side check for the last one | `permanent-failure` `outcome-not-reportable` |
| `failure-reason-missing`, `cancel-reason-required`, `request-invalid`, `expected-version-required` | A request member Orders built wrongly: §4.8 always records a mapped reason, every cancel request carries one (`09 §3.7`), and every call carries the expected version | `permanent-failure` `outcome-not-reportable` |
| `idempotency-mismatch` (409) | The key was settled under a different request | `permanent-failure` `idempotency-key-conflict` |
| An authorization refusal (`operation-not-permitted-for-actor`, `direct-cancel-window-closed`) or `order-not-found` | The Workflow principal's grant or scope is misconfigured, or the order is outside it | `permanent-failure` `not-authorized`, or `not-found` for `order-not-found` |

*Before the call* (`inst-ro-receivers`, decision D-269). The answers of Lifecycle's
`supersede_fulfillment_control` (`bss/orders-stage4` D-259) are taken there, not by this table:
`superseded`, `already_superseded` and `not_pending` settle the control's Lifecycle side and stamp
`lifecycle_superseded_at`; unavailability, a timeout and `still-processing` are
`retryable-failure`; `control-generation-mismatch`, `control-not-supersedable`,
`control-supersede-evidence-mismatch` and `version-conflict` mean the evidence or the control
reference this gear built is wrong — the record, not Lifecycle, is at fault — and answer
`permanent-failure` `outcome-not-reportable`, faulting the invocation like the evidence rows
above. A current-grant read that names an unrecorded generation, a generation whose rows are not
frozen (D-298), or an unattested revoke row, answers the settled `lifecycleCall = revoke-pending` before any of these calls.

## 5. Traceability

- **PRD**: [PRD.md](../PRD.md) §6.4 (Saga and Compensation), Acceptance Criteria 9, 9a, 10, 11; §6.1 `cpt-cf-bss-orders-workflow-fr-owf-process-state-nonauth` (the saga log is Orders')
- **Orders Lifecycle seam**: [06-workflow-seam.md](../../../orders/docs/features/06-workflow-seam.md) §3.3 (`fulfillment-acknowledgement`, `workflow-cancel`), §4.4 (Acknowledgement, normative — compensation evidence and no-Billing-wait rule, bound by reference, not restated)
- **ADRs**: `cpt-cf-bss-orders-workflow-adr-saga-compensable-no-pivot` (as amended by ADR-0011), `cpt-cf-bss-orders-workflow-adr-idempotency-key-composition`, `cpt-cf-bss-orders-workflow-adr-outbox-process-events`,
  [`ADR/0011`](../ADR/0011-cpt-cf-bss-orders-workflow-adr-flow-as-platform-definition.md)
  `cpt-cf-bss-orders-workflow-adr-flow-as-platform-definition`,
  [`ADR/0012`](../ADR/0012-cpt-cf-bss-orders-workflow-adr-definition-versioning-and-protected-steps.md)
  `cpt-cf-bss-orders-workflow-adr-definition-versioning-and-protected-steps`,
  [`ADR/0013`](../ADR/0013-cpt-cf-bss-orders-workflow-adr-references-not-payloads.md)
  `cpt-cf-bss-orders-workflow-adr-references-not-payloads`
- **Definition**: [10-process-definition.md](./10-process-definition.md) §3.6 (c)
  `cpt-cf-bss-orders-workflow-seq-def-partial-failure` (`compensateOrder`, the fragment that
  sequences these operations), (d) `cpt-cf-bss-orders-workflow-seq-def-cancel`, (f)
  `cpt-cf-bss-orders-workflow-seq-def-amendment-and-terminal`, (b) (`reportCompleted`); §4.1 (the
  fence, Unwind row), §4.6 (no swallowing catch)
- **Platform**: [serverless-runtime DESIGN](../../../../serverless-runtime/docs/DESIGN.md) *Compensation Design — Two-Layer Model* (not used as the mechanism; safety-net ask)
- **Prior slices**: [01-foundation.md](./01-foundation.md) (envelope, step-operation contract, `settle-from-lookup`, phase table, reason catalogue), [02-triggers-and-start.md](./02-triggers-and-start.md) (supersede and terminal-event entry, report modes), [03-approval-execution.md](./03-approval-execution.md) (closure port), [04-fulfillment-plan.md](./04-fulfillment-plan.md) (frozen plan, `activation_deadline_at`, completion predicate), [05-provisioning-intents.md](./05-provisioning-intents.md) (intent port, `reconcile-intent`, `execution_seq`, unmatched confirmations, `SUB-O12`), [07-manual-tasks.md](./07-manual-tasks.md) (creation port, Incident Recorder, verified override), [08-hold-and-cancel.md](./08-hold-and-cancel.md) (`authorize-cancel`, cancel request record)
- **Retired here** (ADR-0011): the `compensate-order-fulfillment` and `run-cancellation-fence` process-engine commands (now step routes); the executor's bespoke inter-leg waiting and engine-retry bound; the fencer's step-3 waiting and step-4 driving; slice 08's call to `workflow-cancel` (now `report-outcome`'s)
