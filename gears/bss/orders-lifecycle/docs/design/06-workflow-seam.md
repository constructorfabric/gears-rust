<!-- CONFLUENCE_TITLE: [BSS]: Orders Lifecycle — The Workflow Seam (Slice 6) -->
<!-- Related: ../DESIGN.md, ../PRD.md, ./README.md, ./01-foundation.md | Owners: BSS Orders team -->

# DESIGN — The Workflow Seam (Slice 6)


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
  - [4.1 The five operations are ordinary transitions (normative)](#41-the-five-operations-are-ordinary-transitions-normative)
  - [4.2 Verdicts and the deciding authority (normative)](#42-verdicts-and-the-deciding-authority-normative)
  - [4.3 Begin fulfillment and the spawn signal (normative)](#43-begin-fulfillment-and-the-spawn-signal-normative)
  - [4.4 Acknowledgement (normative)](#44-acknowledgement-normative)
  - [4.5 The per-line projection is not a state machine (normative)](#45-the-per-line-projection-is-not-a-state-machine-normative)
  - [4.6 The upstream asks this slice depends on](#46-the-upstream-asks-this-slice-depends-on)
- [5. Traceability](#5-traceability)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-design-workflow-seam`

## 1. Architecture Overview

### 1.1 Architectural Vision

This slice owns the five operations the sibling **Orders Workflow** gear calls, and it is where
the seam rules R1 through R5 stop being principles and become code
([`../PRD.md`](../PRD.md) §6.1, §6.4).

Its central claim is that those five operations are **ordinary guarded transitions**. There is no
privileged interface, no state-setting bypass, no trusted caller path. Workflow gets the same
authorization pre-guard, the same state-table admissibility check, the same optimistic version
check and the same idempotency contract as a partner admin clicking submit. That is the entire
mechanism behind R1: the gear cannot hold divergent state because there is no way to assert
state without passing a guard.

Two facts recorded here carry disproportionate weight. The **spawn signal** — set when Workflow
reports its first activation intent — is what the cancellation guard reads, which is why
begin-fulfillment must be durably committed *before* Workflow issues any intent; the ordering is
a requirement, not an implementation detail, and without it the guard is racy. And the **deciding
authority** stored alongside every approval verdict is what keeps a stand-in decision
distinguishable from a policy decision after the fact — necessary because the approval policy
owner does not exist yet and its stand-in returns "approval not required" for everything.

The slice records outcomes and refuses to interpret them. It stores a verdict without evaluating
it (R2), holds no provisioning adapter (R3), performs no price arithmetic (R4), and treats the
downstream transition-request identifier as an opaque join key that no projection reads for
state (R5).

### 1.2 Architecture Drivers

#### Functional Drivers

| Requirement | Design Response |
|-------------|------------------|
| `cpt-cf-bss-orders-lifecycle-fr-orders-boundary-r1-state-sor` | All five operations are transition-table rows with the standard guard, version and idempotency contract. No privileged path exists, so divergent state is unrepresentable rather than merely forbidden. |
| `cpt-cf-bss-orders-lifecycle-fr-orders-boundary-r2-approval` | The verdict is stored with its deciding authority and never evaluated. A stand-in decision is a distinguishable value, not an indistinguishable default. |
| `cpt-cf-bss-orders-lifecycle-fr-orders-boundary-r3-provisioning` | No adapter to Subscriptions, the Policy Engine or OSS exists in this gear. Fulfillment outcome arrives only as an acknowledgement. |
| `cpt-cf-bss-orders-lifecycle-fr-orders-boundary-r5-no-mirroring` | The transition-request identifier is a correlation column on the per-line projection; no guard reads it and no state derives from it. |
| `cpt-cf-bss-orders-lifecycle-fr-order-cancel` | The cancel guard reads the recorded spawn signal rather than the order state, which is why accepting a draft-create does not close the direct-cancel window. |
| `cpt-cf-bss-orders-lifecycle-fr-order-atomic-fulfillment` | Terminals are order-level and atomic. The per-line create/activate result is a read-only projection, deliberately not a state machine. |
| `cpt-cf-bss-orders-lifecycle-fr-order-subscription-linkage` | The per-line line-to-subscription mapping is persisted on acknowledgement and carried in `OrderCompleted`, so acquisition provenance is answerable from the order side. |

#### NFR Allocation

| NFR ID | NFR Summary | Allocated To | Design Response | Verification Approach |
|--------|-------------|--------------|-----------------|----------------------|
| `cpt-cf-bss-orders-lifecycle-nfr-order-idempotency` | Zero duplicate transition effects | All five operations | Each carries the standard idempotency contract, so a Workflow retry after a client-side timeout returns the stored outcome and produces no second event | Concurrency test replaying each operation's key and asserting one durable effect |
| `cpt-cf-bss-orders-lifecycle-nfr-order-audit-completeness` | 100 % of transitions audited | Verdict and outcome recorder | Every operation audits with its actor class, the correlation identifier and the deciding authority | Test asserting each of the five writes an audit row carrying the correlation identifier |
| `cpt-cf-bss-orders-lifecycle-nfr-order-transition-latency` | Commit p95 < 1 s | Begin-fulfillment and spawn-signal report | Begin-fulfillment checks acceptance/payment; its commit changes state and audits. Workflow rechecks activation after this commit, outside Lifecycle's transaction. The separate spawn-signal transition commits its cancellation fence and audit before dispatch | Load test separating re-check latency from each transition's commit latency |
| `cpt-cf-bss-orders-lifecycle-nfr-order-recovery` | RPO zero for `submitted`+ | Spawn-signal report | The spawn-signal write is inside its own transition commit, so a recovered database cannot lose the durable fact the cancel guard depends on | DR test asserting the spawn signal survives with its order |

#### Key ADRs

The seven gear ADRs govern this slice. Two decisions taken here are recorded in the register: **the deciding authority is stored
alongside every verdict** ([`../DECISIONS.md`](../DECISIONS.md) D-73, §4.2), and **the per-line
result is a projection rather than a state machine** (D-74, §4.5).

### 1.3 Architecture Layers

Inherited from [`01-foundation`](./01-foundation.md) §1.3. This slice adds one inbound surface
and, deliberately, no outbound adapter.

## 2. Principles and Constraints

### 2.1 Design Principles

#### The sibling gear is an ordinary caller

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-principle-workflow-is-ordinary-caller`

Workflow's five operations pass the same guards as any other. It receives no privileged
interface, no state-setting bypass and no exemption from the version check. R1 is then a
structural property rather than a rule someone must remember: there is no code path by which
Workflow could hold authoritative order state, because there is no code path by which any caller
can set state without a guard.

#### Store the verdict, never the reasoning

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-principle-store-verdict-not-reasoning`

An approval verdict is persisted as a received fact with its deciding authority. This gear does
not evaluate it, does not cache a threshold, does not recompute it on amendment, and does not
infer a later verdict from an earlier one. A denied verdict's `denial_reason` is stored the same
way — as an opaque received fact, never parsed, classified or evaluated (D-135). The corollary
matters while the policy owner is missing: a stand-in decision must be **visibly** a stand-in, or the audit trail will later be
unable to distinguish "policy said no approval was needed" from "nothing was asked".

#### Commit the guard anchor before the risk

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-principle-commit-anchor-before-risk`

Begin-fulfillment must be durably committed before Workflow issues any activation intent,
because the cancel guard reads a fact this gear stores. If the intent could precede the commit,
a cancel arriving in the window would be admitted against an order whose subscriptions were
already activating.

#### An outcome is not a mirror

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-principle-outcome-not-mirror`

This gear records the **order-level outcome** of downstream work and a per-line projection of it.
It does not mirror the Subscriptions transition-request machine, does not reflect
subscription-level approval holds, and stores the downstream request identifier only as a join
key. Mirroring would recreate the dual-source-of-record hazard that R1 exists to prevent, aimed
at Subscriptions instead of Workflow.

### 2.2 Constraints

#### The approval policy owner does not exist

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-constraint-approval-owner-absent`

The Generic Approval service has no authored specification, and until it exists the sibling gear
invokes a stand-in that returns "approval not required" for every order. This gear therefore
stores a verdict it cannot validate. The only defence available is the recorded deciding
authority, which **MUST** be populated on every stored verdict. The built BSS gears' local
approval surfaces are **not** a precedent this gear may follow — a module-local policy owner here
would undo R2.

#### The compensation cancel reason is unagreed

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-constraint-compensation-reason-unagreed`

The acknowledgement path depends on Subscriptions carrying a cancellation reason for
order-fulfillment compensation — registered upstream as `SUB-O1`, marked critical there, and
unagreed. Its absence does not block this slice: the acknowledgement records the compensation
evidence Workflow supplies. But without it a compensating cancel downstream is
indistinguishable from an early termination, which would wrongly derive a termination fee or
credit. The upstream note is explicit that reason values ride event payloads consumers key on,
so adding one after Billing consumes the contract is a breaking change — this is the seam
materially cheaper now than later.

#### Order-reference provenance is not yet bidirectional

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-lifecycle-constraint-provenance-one-directional`

This gear persists the line-to-subscription mapping, so "which subscription did this order
produce" is answerable. The reverse — "which order produced this subscription" — requires
Subscriptions to accept an order reference on `create`, registered upstream as `SUB-O2` and
unagreed. Until it lands, provenance is answerable from one side only, and a subscription created
outside the order path is indistinguishable from one created through it.

#### Correlation propagation is not guaranteed

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-lifecycle-constraint-correlation-propagation-unagreed`

The process correlation identifier is recorded on every audit entry here, which makes the order
side of an acquisition traceable. Propagation onward through Subscriptions to the Policy Engine
and OSS is cited by Workflow as `SUB-O9`, absent from the Subscriptions seam map, and unagreed, so an end-to-end trace currently stops
at the seam.

## 3. Technical Architecture

### 3.1 Domain Model

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-entity-approval-reflection`

A stored approval fact: the verdict — approval required, not required, granted or denied — the
deciding authority that produced it, the denial reason as received where the verdict is denied, the order version it was decided against, and the instant
it was reflected. Never evaluated, never recomputed.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-entity-spawn-signal`

The recorded instant at which Workflow reported its first activation intent for the current
fulfillment attempt. A single nullable column on the aggregate, written by begin-fulfillment's
successor report and read by the cancel guard. It is a fact about what has been attempted, not a
state.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-entity-fulfillment-acknowledgement`

The order-level outcome Workflow reports: completed with the per-line subscription mapping, or
failed with a failure reason from the closed enumeration of §4.4 and the compensation evidence,
under the closed schema of `01 §3.7`, asserting that no active subscription remains.

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-lifecycle-entity-line-fulfillment-projection`

The read-only per-line view — created, activated or failed — with the spawned subscription
identifier and the downstream transition-request identifier as a join key. Explicitly not a
state machine: nothing transitions it, and no order state derives from it.

**Relationships**:
- `Order root` → `Approval reflection`: one-to-many across versions; at most one requirement verdict and one gate outcome per version.
- `Order root` → `Spawn signal`: zero-or-one, **written once and never cleared** (§4.3, D-13). The earlier clearing rule was unreachable — a workflow-mediated cancel lands in `cancelled`, which is terminal with no row out, so no subsequent attempt exists to re-close the window for.
- `Order line` → `Line fulfillment projection`: one-to-one, 1:1 with the spawned subscription.

### 3.2 Component Model

This slice realises `cpt-cf-bss-orders-lifecycle-component-workflow-seam`
([`../DESIGN.md`](../DESIGN.md) §3.2) as three internal parts.

#### Verdict reflector

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-component-seam-verdict-reflector`

##### Why this component exists

The approval arc is the one place where an external authority moves this gear's state, and it
must do so without this gear acquiring any opinion about approval.

##### Responsibility scope

The reflection operation for the requirement verdict and for gate outcomes; storage of the
verdict with its deciding authority and its version; and the guard that refuses a reflection
whose version is superseded.

##### Responsibility boundaries

It evaluates no policy, compares no threshold, caches no prior verdict, and never derives a
verdict for an amended version from the version it superseded.

##### Related components (by ID)

- `cpt-cf-bss-orders-lifecycle-component-transition-orchestrator` — depends on
- `cpt-cf-bss-orders-lifecycle-component-versioning` — shares model with

#### Fulfillment coordinator

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-component-seam-fulfillment-coordinator`

##### Why this component exists

Begin-fulfillment and acknowledgement bracket the only window in which this gear's document can
be overtaken by physical reality, and both ends need to be exact.

##### Responsibility scope

Begin-fulfillment with its two guards from [`05-preconditions`](./05-preconditions.md) and the
activation re-check integration contract in [`03-gate-and-pin`](./03-gate-and-pin.md), executed by Workflow after begin-fulfillment; the spawn-signal record;
acknowledgement with the per-line subscription mapping; the compensation-evidence check on a
failure acknowledgement and on every workflow-mediated cancel; and the workflow-mediated cancel.

##### Responsibility boundaries

It performs no retry, no compensation and no provisioning, and it holds no adapter to
Subscriptions or OSS. It does not decide that compensation completed — it verifies that Workflow
asserted it and records the evidence.

##### Related components (by ID)

- `cpt-cf-bss-orders-lifecycle-component-preconditions-money-gate` — depends on
- `cpt-cf-bss-orders-lifecycle-component-gate-and-pin` — shares contract with

#### Line projection maintainer

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-lifecycle-component-seam-line-projection`

##### Why this component exists

Lifecycle retains acknowledged outcomes and subscription linkage. Live intermediate progress
belongs to Workflow's existing PRD §9.1 progress-read operation (per-line tracking is `fr-owf-line-progress`, PRD §6.3), not to
this acknowledgement-only projection. No intermediate Lifecycle update endpoint is introduced.
Until acknowledgement, absent projection data means "not acknowledged", never "not started".

##### Responsibility scope

Maintenance of the per-line projection from acknowledgements; the subscription identifier; and
the transition-request identifier as a join key.

##### Responsibility boundaries

It exposes no transition, defines no per-line terminal, and is read by no guard. Order terminals
remain atomic and order-level.

##### Related components (by ID)

- `cpt-cf-bss-orders-lifecycle-component-read-and-authz` — owns data for

### 3.3 API Contracts

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-interface-seam-ops`

- **Requirement**: `cpt-cf-bss-orders-lifecycle-interface-order-ops`
- **Technology**: REST/OpenAPI via `OperationBuilder`; RFC 9457 problems. Every call carries an idempotency key, the expected version and the process correlation identifier

| Method | Path | Description | Stability |
|--------|------|-------------|-----------|
| `POST` | `/bss-orders-lifecycle/v1/orders/{orderId}/approval-reflection` | Reflect the requirement verdict or a gate outcome | unstable |
| `POST` | `/bss-orders-lifecycle/v1/orders/{orderId}/begin-fulfillment` | `approved → in_fulfillment`; must commit before any activation intent | unstable |
| `POST` | `/bss-orders-lifecycle/v1/orders/{orderId}/spawn-signal` | Record the first activation intent of the current attempt | unstable |
| `POST` | `/bss-orders-lifecycle/v1/orders/{orderId}/fulfillment-acknowledgement` | `completed` with subscription identifiers, or `fulfillment_failed` with compensation evidence | unstable |
| `POST` | `/bss-orders-lifecycle/v1/orders/{orderId}/workflow-cancel` | Cancel from `in_fulfillment`, or from `on_hold` with pre-hold `in_fulfillment`, with attached compensation evidence — always required, before the spawn signal as after it (D-134) | unstable |

A call that omits the expected version, or carries an unparseable one, is rejected at the
boundary with `expected-version-required` before authorization, unaudited and without touching
idempotency (`01 §4.1`, D-112); a workflow trigger has no exemption.

Authorization restricts all five to the configured Workflow service principal (`service` actor
class, `01 §3.7`, D-115) **and** requires the configured Workflow service principal holding the operation-specific PDP
grant (`approval-reflection`, `begin-fulfillment`, `spawn-signal`, `fulfillment-acknowledgement`,
`workflow-cancel`) and target scope of `08 §4.3`. Actor class alone is insufficient: on its own nothing would
distinguish the sibling gear from the Subscriptions or Billing principals that share that class ([`../DECISIONS.md`](../DECISIONS.md) D-33). They are **not** exempt from any guard;
the restriction is on who may call, not on what applies.

**Reasons contributed to the registry**: verdict-authority-missing,
denial-reason-missing, spawn-signal-already-recorded,
acknowledgement-lines-incomplete, acknowledgement-subscription-missing,
acknowledgement-subscription-duplicated, failure-reason-missing,
compensation-evidence-missing, compensation-evidence-incomplete,
prehold-not-in-fulfillment. The cancel-window reason `direct-cancel-window-closed` and the
mandatory-cancel-reason refusal `cancel-reason-required` are owned and registered by
[`07-hold-and-expiry`](./07-hold-and-expiry.md) §3.3 and used here unchanged. Begin-fulfillment
refuses with the specific reasons of [`05-preconditions`](./05-preconditions.md) §3.3, which this
slice composes and does not rename (D-134). `line-execution-failed` and `dependency-graph-invalid`
are values of the closed `failure_reason` enumeration of §4.4, not refusal reasons: no guard
refuses with them. The activation re-check codes — market-divergence and overlap-collision — are
Workflow-supplied failure reasons carried on `acknowledge-failed`; they are owned and registered
by [`03-gate-and-pin`](./03-gate-and-pin.md) §3.3, and this slice does not run the re-check. A
re-check `defer` whose `activation-recheck-retry-budget` is exhausted carries the port's own
unavailable reason — `identity-party-unavailable` or `overlap-presence-unevaluable`, likewise
owned by 03 — on `acknowledge-failed` (D-127). The
stale-verdict condition uses the engine's own `version-conflict` rather
than a seam-local name, because it is the same condition the engine raises for every other caller
([`../DECISIONS.md`](../DECISIONS.md) D-38; [`01-foundation`](./01-foundation.md) §4.2). Every
trigger of this slice's five operations is in the **workflow-trigger class** of `01 §4.1`, for
which the engine checks the version **before** state-table admissibility, so a stale result is
always refused `version-conflict` naming the current version — never `not-admissible` because an
amendment also moved the state ([`../DECISIONS.md`](../DECISIONS.md) D-110).

### 3.4 Internal Dependencies

| Dependency Gear | Interface Used | Purpose |
|-------------------|----------------|----------|
| `toolkit-db` | Runtime-scoped access, via the engine | Verdict, spawn signal, acknowledgement and projection writes inside the transition transaction |
| `orders-workflow` | Inbound calls and consumed events | The sibling gear calls these five operations and consumes the state events. **This gear makes no outbound call to it** |

### 3.5 External Dependencies

None owned here. Workflow performs the activation re-check through the owning SDKs per
[`03-gate-and-pin`](./03-gate-and-pin.md) §3.6; Lifecycle holds no port for it. This slice deliberately holds
no adapter to Subscriptions, the Policy Engine or OSS — that absence is how R3 is enforced rather
than merely asserted.

### 3.6 Interactions and Sequences

#### Reflect an approval verdict

**ID**: `cpt-cf-bss-orders-lifecycle-seq-reflect-verdict`

**Use cases**: `cpt-cf-bss-orders-lifecycle-usecase-order-new-acquisition`

**Actors**: `cpt-cf-bss-orders-lifecycle-actor-orders-workflow`

**Algorithm: Reflect Verdict**

Input: order_id, verdict, deciding_authority, denial_reason (required when the verdict is `denied`, forbidden otherwise), expected_version, idempotency_key, correlation_id
Output: the resulting state, or a registered refusal

1. [ ] - `p1` - Declare three guards the engine evaluates and audits: **deciding_authority present** (`verdict-authority-missing`); **denial_reason present on a denied verdict** (`denial-reason-missing`, D-135) — a denial_reason supplied with any other verdict is rejected at boundary validation with `request-invalid`, before authorization and unaudited (`01 §4.7` *Validation flow at the boundary*, D-142); and expected_version current — the latter is the engine's own `version-conflict`, already evaluated at `01 §3.6` *Attempt Transition* step 10 — ahead of the row lookup, because every reflection trigger is workflow-class (`01 §4.1`, D-110), so a verdict for a superseded version is refused `version-conflict` naming the current version even when the amendment moved the order to `submitted` — so this slice pre-checks none of them and returns no refusal of its own - `inst-rv-declare-guards`
2. [ ] - `p1` - **MATCH** the verdict to a **trigger**, which is what the engine looks the row up by — this slice resolves the trigger and never the target state (`01 §4.6`): - `inst-rv-match-verdict`
   1. [ ] - `p1` - **WHEN** approval required: trigger `reflect-approval-required` - `inst-rv-when-required`
   2. [ ] - `p1` - **WHEN** approval not required: trigger `reflect-approval-not-required` - `inst-rv-when-not-required`
   3. [ ] - `p1` - **WHEN** granted: trigger `reflect-approval-granted` - `inst-rv-when-granted`
   4. [ ] - `p1` - **WHEN** denied: trigger `reflect-approval-denied` - `inst-rv-when-denied`
3. [ ] - `p1` - Classify `required` or `not_required` as `requirement`, and `granted` or `denied` as `gate_outcome`; pass that verdict kind, the verdict, its deciding authority, the denial_reason where the verdict is denied, correlation_id and the version as the contribution to the reflection transition, so the engine writes it inside the same transaction - `inst-rv-contribute-verdict`
4. [ ] - `p1` - **RETURN** the resulting state - `inst-rv-return-state`

**Description**: Step 1's authority guard is the whole defence against an absent policy owner. A
verdict without a named authority is refused — by the engine, so the refusal audits and settles
like every other — and a stand-in decision is therefore recorded as a stand-in decision rather
than being indistinguishable from a policy one. The denial reason is carried for `OrderRejected`
(`01 §4.4`) and stored beside the verdict; like the verdict it is received, never evaluated.

#### Begin fulfillment and record the spawn signal

**ID**: `cpt-cf-bss-orders-lifecycle-seq-begin-and-spawn`

**Use cases**: `cpt-cf-bss-orders-lifecycle-usecase-order-fulfillment-complete`

**Actors**: `cpt-cf-bss-orders-lifecycle-actor-orders-workflow`, `cpt-cf-bss-orders-lifecycle-actor-orders-subscriptions`

```mermaid
sequenceDiagram
    participant W as Orders Workflow
    participant L as Orders Lifecycle
    participant S as Subscriptions
    W ->> L: begin-fulfillment (auth outcome, version)
    L ->> L: acceptance + payment authorization guards
    L ->> L: commit in_fulfillment - DURABLE
    L -->> W: in_fulfillment
    W ->> S: wave 1 - draft-create per line
    S -->> W: drafts created (not resource-affecting)
    W ->> W: after wave-1 drafts, recheck market and overlap via owning SDKs
    Note over W: proceed / reject / not-dispatchable / defer (03 §3.6, D-127)
    W ->> L: spawn-signal only on proceed (fence before activation dispatch)
    L ->> L: record instant - closes the direct-cancel window
    L -->> W: recorded
    W ->> S: wave 2 - activation intents
```

**Description**: The ordering is the design. `in_fulfillment` commits before any intent exists,
and the spawn signal is committed before dispatch of the first *activation* intent — not at draft-create — which
is why a cancel during wave 1 is still admitted and compensated by a cheap draft void.

**Algorithm: Begin Fulfillment**

Input: order_id, authorization_outcome, expected_version, idempotency_key, correlation_id
Output: in_fulfillment, or a registered refusal

1. [ ] - `p1` - Declare the guards the engine evaluates and audits: the caller is the configured Workflow `service` principal (`01 §3.7` *Actor class*, D-115), settled by the engine's authorization pre-guard at `01 §3.6` *Attempt Transition* step 1 against `08 §4.3`; expected_version current — the engine's own `version-conflict` at `01 §3.6` *Attempt Transition* step 10, checked ahead of the row lookup because `begin-fulfillment` is workflow-class (`01 §4.1`, D-110); and, as row 11's guards, the guards of [`05-preconditions`](./05-preconditions.md) §3.6 *Evaluate Begin-Fulfillment Preconditions* — `acceptance-required-not-recorded`, `acceptance-requirement-unevaluable`, `authorization-pending`, `authorization-failed` — composed unchanged; this slice pre-checks none of them and adds no reason of its own - `inst-bf-declare-guards`
2. [ ] - `p1` - Resolve the precondition guards' inputs by composing *Evaluate Begin-Fulfillment Preconditions* with order_id, expected_version, authorization_outcome and the authenticated security context - `inst-bf-compose-preconditions`
3. [ ] - `p1` - **IF** the composed evaluation admits with the risk flag (a `failed` outcome under an elected tolerate-failure): add the tolerance decision to the contribution, from which the engine sets `authorization_failure_tolerated_at` to the server-recorded decision instant (`01 §3.6`, required aggregate writes); otherwise contribute nothing to that column - `inst-bf-contribute-tolerance`
4. [ ] - `p1` - Set the **trigger** to `begin-fulfillment` (`01 §4.3` row 11) and request the begin-fulfillment transition with correlation_id; the engine commits `in_fulfillment` durably, audits and settles the key before it returns - `inst-bf-request-transition`
5. [ ] - `p1` - **RETURN** in_fulfillment; Workflow **MUST NOT** issue any intent before this return (§4.3) - `inst-bf-return`

**Description**: The slice owns no begin-fulfillment rule. It hands the row the guards `05` owns,
so each unmet precondition refuses with its own name rather than a seam-level catch-all, and it
contributes the risk flag only when those guards admitted a tolerated failure.

**Algorithm: Report Spawn Signal**

Input: order_id, expected_version, idempotency_key, correlation_id
Output: the recorded spawn-signal instant, or a registered refusal

1. [ ] - `p1` - Declare the guards the engine evaluates and audits: the caller is the configured Workflow `service` principal, settled by the engine's authorization pre-guard (`08 §4.3`, D-115); expected_version current — `version-conflict`, checked ahead of the row lookup for this workflow-class trigger (`01 §4.1`, D-110); and **`orders_order.spawn_signal_at` IS NULL** (`spawn-signal-already-recorded`) - `inst-ss-declare-guards`
2. [ ] - `p1` - Set the **trigger** to `report-spawn-signal` (`01 §4.3` row 12, `in_fulfillment → in_fulfillment`). A held, cancelled or otherwise non-`in_fulfillment` order has no row for this trigger and receives the engine's state-table refusal `not-admissible` (or `version-conflict` first where its version is also stale); this slice adds no state check of its own - `inst-ss-set-trigger`
3. [ ] - `p1` - Request the spawn-signal transition with correlation_id; the engine sets `spawn_signal_at` to the server-recorded report instant, audits and commits before it returns - `inst-ss-request-transition`
4. [ ] - `p1` - **RETURN** the recorded instant - `inst-ss-return`

**Description**: A replay with the same idempotency key returns the stored outcome under the
engine's idempotency contract and never reaches the already-recorded guard, so a Workflow retry
after a timeout learns that its own report was admitted. `spawn-signal-already-recorded` answers
only a second report under a different key. The committed return is the fence activation
dispatch waits on (§4.3).

#### Acknowledge the fulfillment outcome

**ID**: `cpt-cf-bss-orders-lifecycle-seq-acknowledge-fulfillment`

**Use cases**: `cpt-cf-bss-orders-lifecycle-usecase-order-fulfillment-complete`

**Actors**: `cpt-cf-bss-orders-lifecycle-actor-orders-workflow`, `cpt-cf-bss-orders-lifecycle-actor-orders-subscriptions`

**Algorithm: Acknowledge Fulfillment**

Input: order_id, outcome, per_line_results, compensation_evidence, failure_reason (failed outcome only; supplied with the completed outcome it is `request-invalid` at the boundary, D-142), expected_version, idempotency_key, correlation_id
Output: completed or fulfillment_failed, or a registered refusal

**Admissible from-states.** The failed outcome is admitted from `in_fulfillment` (`01 §4.3` row 14)
**and** from `on_hold` whose stored pre-hold state is `in_fulfillment` (row 26), with the same
evidence guards; the completed outcome is admitted from `in_fulfillment` only (row 13), so a held
order **MUST** be resumed (row 22) before it can be acknowledged completed, and a completed
acknowledgement against a held order carrying the current version is refused `not-admissible` (one
carrying a superseded version is refused `version-conflict` first, `01 §4.1`, D-110). Row 26 carries a registered
pre-hold guard that refuses `prehold-not-in-fulfillment` for a hold from any other state
([`../DECISIONS.md`](../DECISIONS.md) D-109).

1. [ ] - `p1` - Declare the guards the engine evaluates and audits: expected_version current (the engine's own `version-conflict` at `01 §3.6` *Attempt Transition* step 10, checked ahead of the row lookup for these workflow-class triggers — `01 §4.1`, D-110 — so this slice does not pre-check it); on the **completed** outcome, every line carrying an activated result (`acknowledgement-lines-incomplete`), every activated line carrying a subscription identifier (`acknowledgement-subscription-missing`), and those identifiers being distinct across the order's lines (`acknowledgement-subscription-duplicated`); on the **failed** outcome, a failure reason present (`failure-reason-missing`, D-136) — a value outside the closed enumeration of §4.4, or a failure_reason supplied with the completed outcome, is rejected at boundary validation with `request-invalid` and never reaches the engine (`01 §4.7`, D-142) — compensation evidence present (`compensation-evidence-missing`) and valid against the closed schema of `01 §3.7` with `no_active_subscription_remains` true (`compensation-evidence-incomplete`), and — where the order is `on_hold` — a stored pre-hold state of `in_fulfillment` (`prehold-not-in-fulfillment`, row 26) - `inst-af-declare-guards`
2. [ ] - `p1` - **MATCH** the outcome: - `inst-af-match-outcome`
   1. [ ] - `p1` - **WHEN** completed: - `inst-af-when-completed`
      1. [ ] - `p1` - Read the authoritative roster from `orders_order_line` for `(order_id, expected_version)`, joined to `orders_order_line_identity`; require exactly one result per roster line and no unknown or duplicate line IDs, then require every result activated with a non-null subscription ID and all subscription IDs distinct. Missing, extra, duplicate or non-activated line results yield `acknowledgement-lines-incomplete`; missing or duplicate subscription IDs use their dedicated reasons. Never derive the expected roster from the payload or mutable fulfillment projection - `inst-af-resolve-completed-inputs`
      2. [ ] - `p1` - Add the per-line line-to-subscription mapping to the contribution - `inst-af-contribute-linkage`
      3. [ ] - `p1` - Set the **trigger** to `acknowledge-completed`; the engine resolves the target from the row (`01 §4.6`) - `inst-af-target-completed`
   2. [ ] - `p1` - **WHEN** failed: - `inst-af-when-failed`
      1. [ ] - `p1` - Resolve the failure-reason and evidence guards' inputs from failure_reason and compensation_evidence, validating structure only and never reconciling the lists against Subscriptions (`01 §3.7`); where the reason is missing or the evidence is absent, schema-invalid or does not assert that no active subscription remains, the engine refuses and the order stays in its current state (`in_fulfillment`, or `on_hold` for row 26) - `inst-af-resolve-evidence-inputs`
      2. [ ] - `p1` - Add the compensation evidence and the failure reason to the contribution; the engine persists the evidence, records the failure reason on the committed audit entry as its `caller_reason` — its `reason` stays the registered machine reason (`01 §3.7`, D-143) — and carries both in `OrderFulfillmentFailed` - `inst-af-contribute-evidence`
      3. [ ] - `p1` - Set the **trigger** to `acknowledge-failed`; the engine resolves the target from the row (`01 §4.6`) — row 14 from `in_fulfillment`, row 26 from `on_hold` with pre-hold `in_fulfillment` - `inst-af-target-failed`
3. [ ] - `p1` - Add the per-line projection update to the contribution - `inst-af-contribute-projection`
4. [ ] - `p1` - Request the acknowledgement transition with correlation_id, which writes every contribution and audits in one transaction - `inst-af-request-transition`
5. [ ] - `p1` - **RETURN** the resulting terminal state - `inst-af-return-terminal`

**Description**: Step 2.2 is the invariant that ties the two gears together: `fulfillment_failed`
presupposes completed operational compensation, so an acknowledgement that cannot assert it
leaves the order non-terminal under the sibling gear's escalation SLA. The order never waits on a
Billing credit note — money reverse is a billing-chain concern.

#### Cancel across the spawn boundary (the shared cancel guard)

**ID**: `cpt-cf-bss-orders-lifecycle-seq-seam-cancel-guard`

**Use cases**: `cpt-cf-bss-orders-lifecycle-usecase-order-cancel-during-approval`

**Actors**: `cpt-cf-bss-orders-lifecycle-actor-orders-seller-operator`, `cpt-cf-bss-orders-lifecycle-actor-orders-workflow`

**Algorithm: Evaluate Cancel From In-Fulfillment (shared guard)**

**This is a shared guard, not the `/workflow-cancel` handler.** Both cancel entry points defer to
it for an order in `in_fulfillment`, or in `on_hold` whose stored pre-hold state is
`in_fulfillment`: the **ordinary** `POST /cancel` owned by
[`07-hold-and-expiry`](./07-hold-and-expiry.md) §4.6 *Cancel Order* step 2, and the
**workflow-mediated** `POST /workflow-cancel` declared in §3.3. It therefore **MUST NOT** be read
as the authorization boundary of either path. Who may call each operation is settled **before**
this guard runs, by the permission matrix of [`08-read-and-authz`](./08-read-and-authz.md) §4.3
enforced as the engine's authorization pre-guard ahead of every slice guard
(`01 §4.1` *Guard evaluation order*, `01 §3.6` *Attempt Transition* step 1) — and that matrix
restricts `/workflow-cancel` to the Workflow service principal. What `requesting_actor_class`
decides **here** is only *which cancel window applies* to a caller already authorized for the
operation they invoked, which is why step 3 reads it after step 2 rather than before.

Input: order_id, requesting_actor_class and requesting actor (of the already-authorized caller,
from its authenticated context and never from the request — `01 §3.7` *Actor class*; supplied by
whichever cancel entry point invoked this guard)
Output: admit or a registered refusal

1. [ ] - `p1` - Read the recorded spawn signal - `inst-cg-read-spawn-signal`
2. [ ] - `p1` - **IF** no spawn signal is recorded: - `inst-cg-if-no-spawn`
   1. [ ] - `p1` - **RETURN** admit; the direct-cancel window is still open, so **either** entry point's caller may cancel and wave-1 drafts are compensated by void. This admits on the window, not on the actor: it is not an authorization decision and does not widen who may call `/workflow-cancel` - `inst-cg-return-direct-admit`
3. [ ] - `p1` - **IF** the requesting actor is not the configured Workflow service principal (`service` class, `01 §3.7`, D-115): - `inst-cg-if-not-workflow`
   1. [ ] - `p1` - **RETURN** direct-cancel-window-closed refusal - `inst-cg-return-window-closed`
4. [ ] - `p1` - **RETURN** admit - `inst-cg-return-mediated-admit`

**Description**: The guard reads a recorded fact rather than inferring from state, so the window
closes exactly when the first activation intent is reported and not when `in_fulfillment` is
entered. It holds only the window and actor logic. The compensation-evidence guard is not part
of it: it is registered on the `cancel-workflow-mediated` rows and applies whether or not the
spawn signal is recorded (*Workflow Cancel* below, D-134), so the ordinary `POST /cancel` of
`07 §4.6`, which carries no evidence, is unaffected. After `completed` there is no order-side cancellation window at all — post-purchase
rights are exercised on the spawned subscriptions.

Because the guard is shared, the step order is deliberate and **MUST NOT** be rewritten as an
actor check ahead of the window check: hoisting step 3 above step 2 would make the ordinary
`POST /cancel` of `07 §4.6` refuse every pre-spawn cancellation by a seller operator or partner
admin — the cancel path PRD §6.3 requires — and it would not add any authorization the engine
pre-guard does not already enforce. The two callers are distinguished by **permission** at the
pre-guard and by **window** here, and those are separate concerns
([`../DECISIONS.md`](../DECISIONS.md) D-33).

**`/workflow-cancel` is admitted from a hold as well.** It resolves to `cancel-workflow-mediated`
from `in_fulfillment` (`01 §4.3` row 16) **and** from `on_hold` whose stored pre-hold state is
`in_fulfillment` (row 27); a hold from any other state is refused `prehold-not-in-fulfillment` by
the row's registered pre-hold guard, which rows 26 and 27 share. `/workflow-cancel` always requires
complete compensation evidence; before the spawn signal the evidence records the voided drafts and
`activation_dispatched = false` (§4.3).
Row 27 is what lets Workflow close an order it held (`on_hold` plus a manual task) after the spawn
signal once the resume cap of `07 §4.1` is exhausted, where the ordinary cancel of row 23 refuses
every other caller ([`../DECISIONS.md`](../DECISIONS.md) D-109).

#### Cancel through the Workflow (the `/workflow-cancel` handler)

**ID**: `cpt-cf-bss-orders-lifecycle-seq-workflow-cancel`

**Use cases**: `cpt-cf-bss-orders-lifecycle-usecase-order-cancel-during-approval`

**Actors**: `cpt-cf-bss-orders-lifecycle-actor-orders-workflow`

**Algorithm: Workflow Cancel**

Input: order_id, compensation_evidence, cancel_reason, expected_version, idempotency_key, correlation_id
Output: cancelled, or a registered refusal

1. [ ] - `p1` - Declare the guards the engine evaluates and audits: the caller is the configured Workflow `service` principal, settled by the engine's authorization pre-guard against `08 §4.3` (D-115); expected_version current — `version-conflict`, checked ahead of the row lookup for this workflow-class trigger (`01 §4.1`, D-110); a **mandatory** cancel reason (`cancel-reason-required`, owned by `07 §3.3` and reused unchanged, since `07 §4.6` makes the reason mandatory for every actor); where the order is `on_hold`, a stored pre-hold state of `in_fulfillment` (`prehold-not-in-fulfillment`, row 27); compensation evidence present (`compensation-evidence-missing`) and valid against the closed schema of `01 §3.7` with `no_active_subscription_remains` true (`compensation-evidence-incomplete`), required **whether or not the spawn signal is recorded** (D-134); and the shared cancel guard above - `inst-wc-declare-guards`
2. [ ] - `p1` - Resolve the shared cancel guard's inputs: *Evaluate Cancel From In-Fulfillment (shared guard)* with the Workflow principal's actor class `service` and its subject, both from the authenticated context and never from the request; before the spawn signal it admits on the open window, after it on the Workflow actor - `inst-wc-shared-guard`
3. [ ] - `p1` - Resolve the evidence guard's inputs from compensation_evidence, validating structure only and never reconciling the lists against Subscriptions (`01 §3.7`) - `inst-wc-resolve-evidence`
4. [ ] - `p1` - Add the compensation evidence and the cancel reason to the contribution - `inst-wc-contribute-evidence`
5. [ ] - `p1` - Set the **trigger** to `cancel-workflow-mediated`; the engine resolves the row (`01 §4.6`) — row 16 from `in_fulfillment`, row 27 from `on_hold` with pre-hold `in_fulfillment` - `inst-wc-set-trigger`
6. [ ] - `p1` - Request the workflow-mediated cancel transition with correlation_id, which persists the evidence, records the cancel reason in the audit entry's `caller_reason` (D-143) and publishes `OrderCancelled` in one transaction - `inst-wc-request-transition`
7. [ ] - `p1` - **RETURN** cancelled - `inst-wc-return`

**Description**: The evidence requirement does not depend on the spawn signal. Before it, the
evidence lists the voided wave-1 drafts (possibly none), an empty `activated_rolled_back` and
`activation_dispatched = false`; after it, the rolled-back subscriptions as well. Either way
`no_active_subscription_remains` must be true, which is what row 16 and PRD's compensation
contract both require.

### 3.7 Database Schemas and Tables

This slice introduces one table, `orders_approval_reflection`, specified here. It also owns
`orders_line_fulfillment` (`cpt-cf-bss-orders-lifecycle-dbtable-line-fulfillment`) and writes the
`spawn_signal_at` column on `orders_order`; those two are specified in
[`01-foundation`](./01-foundation.md) §3.7.

#### Table: orders_approval_reflection

**ID**: `cpt-cf-bss-orders-lifecycle-dbtable-approval-reflection`

**Schema**:

| Column | Type | Description |
|--------|------|-------------|
| reflection_id | uuid | Entry identity |
| order_id | uuid | Owning aggregate |
| version | integer | The order version the verdict was decided against |
| verdict_kind | enum | `requirement` or `gate_outcome`; one reflected fact of each kind may stand for a version |
| verdict | enum | `required`, `not_required`, `granted` or `denied`; admissible values are constrained by `verdict_kind` |
| deciding_authority | text | The named authority — the policy owner, or the stand-in, explicitly |
| denial_reason | text, nullable | The reason received with a denied gate outcome, stored as an opaque fact and never evaluated; carried in `OrderRejected` (D-135) |
| correlation_id | uuid, NOT NULL | Mandatory sibling process correlation, copied from the validated reflection request by the engine in the same transaction |
| reflected_at | timestamptz | Reflection instant |

**PK**: reflection_id

**Constraints**: append-only; FK `(order_id, version)` → `orders_order_version`
(`01 §3.7`); `deciding_authority` NOT NULL, which is what makes a stand-in
decision permanently distinguishable; **`(order_id, version, verdict_kind)` UNIQUE**, so exactly
one requirement verdict and one gate outcome may stand per version. `requirement` admits only
`required` or `not_required`; `gate_outcome` admits only `granted` or `denied`
([`../DECISIONS.md`](../DECISIONS.md) D-28). A CHECK holds `denial_reason` NOT NULL exactly when
`verdict` = `denied`, and NULL on every other verdict (D-135).

**Compensation evidence** is stored as `orders_order.compensation_evidence` (jsonb), written by the
failure-acknowledgement or workflow-cancel transition, under the closed five-member schema of
[`01-foundation`](./01-foundation.md) §3.7: `drafts_voided`, `activated_rolled_back`,
`activation_dispatched`, `at_sale_facts_emitted` and `no_active_subscription_remains`. Lifecycle
validates that structure only and never reconciles the lists against Subscriptions.

**Additional info**: superseded versions' reflections are retained, so the trail shows what was
decided against a version that no longer stands.

Two notes on the tables owned elsewhere. `orders_order.spawn_signal_at` is written by the
**spawn-signal transition** and never cleared. And
`orders_line_fulfillment.transition_request_ref` is a **join key with no state semantics** — no
guard reads it, and no projection derives order state from it, which is R5 expressed as a schema
rule.

### 3.8 Deployment Topology

Inherited from [`01-foundation`](./01-foundation.md) §3.8. No background worker. The overdue
escalation for an order stuck in `in_fulfillment` is raised by the sibling gear, not here.

**Observability owned here**: per-operation call rate, latency and refusal breakdown for all five
seam operations; the count of verdicts recorded against the **stand-in** authority versus a real
policy owner, which is the signal that approval is still unimplemented; version-conflict rate on
reflections and acknowledgements, which measures how often amendments are racing approvals (every
such race lands here, because the version is checked before admissibility for these triggers, D-110);
acknowledgements refused for incomplete compensation evidence; and the interval between
begin-fulfillment and the spawn-signal report, which is the window the direct-cancel guard is open
for. Alerts fire on a non-zero rate of evidence-incomplete acknowledgements, because each one
means an order is sitting non-terminal awaiting operator action.

## 4. Additional Context

### 4.1 The five operations are ordinary transitions (normative)

Approval reflection, begin fulfillment, **the spawn-signal report**, fulfillment acknowledgement
and the workflow-mediated cancel **MUST** each be a transition-table row subject to the full guard
order — including its one declared exception, that for these workflow-class triggers the version
check precedes state-table admissibility (`01 §4.1`, D-110) — the optimistic version check and the
standard idempotency contract. The spawn-signal report
is row 12 of [`01-foundation`](./01-foundation.md) §4.3, a state-only `in_fulfillment →
in_fulfillment` transition — it was previously outside the table and therefore outside this
sentence, which is what let it escape the guard order
([`../DECISIONS.md`](../DECISIONS.md) D-11). Authorization **MUST** restrict them to the
authenticated Workflow service principal with the operation-specific PDP grant and target scope
(`08 §4.3`); actor class alone is insufficient. That restriction **MUST NOT** exempt it from any
guard.

Two of them are rows from `on_hold` as well: fulfillment acknowledgement's failed outcome
(`acknowledge-failed`, row 26) and the workflow-mediated cancel (`cancel-workflow-mediated`,
row 27) are admitted from `on_hold` only when the stored pre-hold state is `in_fulfillment`, under
the guards of rows 14 and 16 unchanged. `acknowledge-completed` has no `on_hold` row, so a held
order **MUST** be resumed before it completes ([`../DECISIONS.md`](../DECISIONS.md) D-109).

No operation **MAY** be added that sets order state without passing a guard, for any purpose
including data repair. A repair need becomes a new transition row with its own guard and reason.

### 4.2 Verdicts and the deciding authority (normative)

Every stored verdict **MUST** carry a named **deciding authority** and the order version it was
decided against, and a reflection lacking an authority **MUST** be refused. This gear **MUST
NOT** evaluate a verdict, compare a threshold, cache a prior verdict, or derive a verdict for an
amended version from the version it superseded.

**Re-approval after an amendment.** An amendment lands the order in `submitted` and publishes
`OrderAmended` ([`04-versioning`](./04-versioning.md) §4.3). On consuming that event the sibling
gear **MUST** obtain the requirement verdict for the **new** version and reflect the order onward
via `submitted → pending_approval` or `submitted → approved`. It **MUST NOT** carry the prior
version's verdict forward, which is the derivation the paragraph above forbids. Until that
reflection lands the order sits in `submitted`, and the `submitted` TTL continues to run — an
amendment does not pause the clock.

A **denied** verdict **MUST** carry a denial reason, and a reflection of `denied` without one
**MUST** be refused `denial-reason-missing`; no other verdict carries one. The reason is stored on
the reflection and published in `OrderRejected` as an opaque received fact. This gear **MUST NOT**
parse, classify or evaluate it (D-135).

While the approval policy owner is unspecified, the sibling gear invokes a stand-in returning
"approval not required" for every order. The stand-in **MUST** be recorded as the authority by
name. Without that, a later audit cannot distinguish an order that policy exempted from an order
nobody ever asked about — and given the stand-in currently exempts everything, that distinction
is the entire audit value of the field.

### 4.3 Begin fulfillment and the spawn signal (normative)

Begin fulfillment **MUST** be durably committed before Workflow issues any activation intent.
The spawn signal **MUST** commit before Workflow dispatches its **first activation intent** of
the current attempt, and **MUST NOT** be recorded on draft-create acceptance. This local commit
is the single **order direct-cancel** boundary; receipt or acceptance by Subscriptions is not
that boundary. Direct cancel and spawn-signal serialize through the same aggregate transaction:
if cancellation commits first, the signal refuses and Workflow dispatches nothing; if the
signal commits first, direct cancellation refuses and cancellation follows the mediated path.
A timeout is not permission to dispatch: Workflow resolves/replays the same idempotency key
until signal admission is known. If it crashes after admission but before dispatch, the fence
remains durable and Workflow reconciles its own dispatch checkpoint; mediated cancellation can
complete with evidence that no activation was dispatched and all drafts were voided.

The Workflow PRD §6.4 currently describes a `FulfillmentTask` as unilaterally cancellable until
Subscriptions accepts its activation intent. That **task** cancellation point must not be
advertised as the order's direct-cancel point. The PRD parenthesis equating that acceptance with
Lifecycle's spawn signal must be corrected before integration: a post-acceptance signal would
leave an unsafe interval admitting order cancellation while activation is already accepted.
The pre-dispatch boundary is the proposed seam reconciliation, not a claim that the existing
neighbor PRD already agrees. No distributed transaction or new platform fence service is needed.

Begin-fulfillment checks current-version acceptance and payment authorization, then commits
`in_fulfillment`. Workflow checks overlap at plan construction and rechecks overlap and market
after wave-1 drafts, before `report-spawn-signal` and activation dispatch. It uses the owning
identity and Subscriptions SDK contracts described in `03 §3.6`, not a new Lifecycle
endpoint or a caller-supplied pass flag. Missing public SDK operations remain prerequisites.
The Lifecycle-side inputs — each line's stored `overlap_scope_key` and the version's market and
payer — come from the composed read of [`08-read-and-authz`](./08-read-and-authz.md) §4.2, which
carries them as fulfillment inputs ([`../DECISIONS.md`](../DECISIONS.md) D-144).
The re-check returns exactly one of four outcomes (`03 §3.6` *Re-check Activation
Preconditions*, D-127), and each drives at most one transition of this slice:

* **`proceed`** — Workflow requests `report-spawn-signal` (row 12) and then dispatches the activation wave.
* **`reject`** (per-line `market-divergence` or `overlap-collision`) — Workflow voids any wave-1 drafts and calls `acknowledge-failed` (row 14) from `in_fulfillment`; before any draft exists, compensation evidence identifies an empty created set.
* **`not-dispatchable`** (the order is held, terminal or superseded) — Workflow stops dispatch, **does not** call acknowledge and drives no transition; it re-reads the order, waits for resume (row 22) and re-runs the re-check if held, follows the current version if superseded, and ends the attempt if terminal.
* **`defer`** (the identity port or the overlap-occupancy port was unavailable) — Workflow retries the re-check from its first step with bounded backoff under `activation-recheck-retry-budget` (baseline 3 attempts over ≤ 60 s, `03 §3.6`) and drives no transition while retrying; once the budget is exhausted it voids the wave-1 drafts and calls `acknowledge-failed` (row 14) carrying the port's unevaluable reason. Unavailable inputs **MUST NOT** be fabricated as market divergence or collision.

**It still has an order-level outcome, and that outcome is row 14.** PRD §6.1 requires market
divergence and overlap collision at activation to be "surfaced as a fulfillment failure", and
without a named route the order would sit in `in_fulfillment` indefinitely — a non-terminal state
the design deliberately exempts from expiry, so nothing would ever move it. Workflow therefore
acknowledges failure through `acknowledge-failed` (row 14), carrying compensation evidence. The
evidence requirement is satisfiable by construction here: the re-check runs **before** the first
activation intent, so no subscription was ever activated and the evidence records the voided wave-1
drafts and asserts that no active subscription remains. No new transition row is needed — the
existing failure path already expresses this, and routing it explicitly is what closes the gap.
The refusing predicate's reason (`market-divergence` or `overlap-collision`) is carried as the
`failure_reason` (§4.4) so the cause survives on the audit entry, in its `caller_reason` (`01 §3.7`, D-143).

**The activation re-check verdict is an early abort, and it does not expire.** This gear cannot
make the check atomic with the transaction that commits a subscription to `active`, and it also
cannot express a deadline: the re-check returns one of its four outcomes to its caller, none carrying a validity origin, and
the transition Workflow then drives — `report-spawn-signal`, `01 §4.3` row 12 — is event-less, so no
declared interface carries a validity origin or a window (`03 §2.2`, D-89). What Workflow **MUST**
do is narrower and checkable: **MUST NOT** treat a proceed verdict as an admission guarantee, and **MUST** handle
an `overlap-collision` raised by Subscriptions at any point after the re-check — including after
lines the two-phase barrier deferred, which is precisely the case one window could never have
covered. Such a collision arrives on the failure-acknowledgement path of §4.4 with compensation
evidence, rather than as a silent partial activation
([`../DECISIONS.md`](../DECISIONS.md) D-89).

**The activation intent carries the start instant.** Each activation intent **MUST** carry the
**actual activation instant** as the spawned subscription's start, and **MUST NOT** derive that
start from any date carried on the order. Where the two-phase barrier defers a line past its
quoted service-activation date, the quoted date travels separately as the *requested* date and
the subscription's start is the instant activation actually occurred — billing and entitlement
**MUST NOT** be backdated to the earlier quoted date ([`../PRD.md`](../PRD.md) §6.1, §12 AC 8g).
This gear cannot enforce it alone, because Subscriptions owns the start: the obligation is raised
as [`../UPSTREAM_REQS.md`](../UPSTREAM_REQS.md) `…-upreq-subscription-start-instant` (**`SUB-O10`**),
and until it lands the requirement is stated here and unenforceable from this side
([`../DECISIONS.md`](../DECISIONS.md) D-56).

The signal is **written once and never cleared**. The previous rule cleared it on a
workflow-mediated cancel "so a subsequent attempt re-closes the window" — but that cancel lands in
`cancelled`, which is terminal with no row out, so no subsequent attempt exists and the rule was
unreachable ([`../DECISIONS.md`](../DECISIONS.md) D-13).

### 4.4 Acknowledgement (normative)

A **completed** acknowledgement **MUST** report every line as activated and **MUST** carry a
subscription identifier for each, and those identifiers **MUST** be **distinct across the order's
lines**. Completeness is exact set equality against the immutable current-version
`orders_order_line` roster joined to `orders_order_line_identity`, with multiplicity one for each
line ID; an empty payload for a nonempty order, omitted line, foreign/unknown line, repeated line
ID, or non-activated result **MUST** fail `acknowledgement-lines-incomplete`. The engine's final
expected-version check binds this roster to the version being completed. No projection is read
by this guard. An incomplete report, or one reusing a subscription identifier, **MUST** be refused rather than
partially applied. Distinctness is what makes the 1:1 line-to-subscription mapping an enforced
invariant rather than a documented expectation: requiring *an* identifier per line permits one
subscription to answer for two lines, which is exactly the composition Q-02 asked about and D-84
declined ([`../DECISIONS.md`](../DECISIONS.md) D-84). The per-line line-to-subscription mapping **MUST** be persisted and **MUST** be carried
in `OrderCompleted`.

A **failed** acknowledgement **MUST** carry compensation evidence asserting that no active
subscription remains, and **MUST** be refused where the evidence is absent or does not assert it
— leaving the order in `in_fulfillment` (or `on_hold`, for row 26), non-terminal, under the sibling
gear's escalation SLA. A failed acknowledgement **MAY** be made from `on_hold` with pre-hold
`in_fulfillment` (row 26); a completed one **MUST NOT**, and requires resume first (D-109).
The evidence **MUST** follow the closed schema of `01 §3.7` — which drafts were voided, which
activated subscriptions were rolled back, whether activation was dispatched, whether at-sale
billable facts had been emitted, and the assertion that no active subscription remains. Absent
evidence **MUST** be refused `compensation-evidence-missing`; evidence that fails the schema or
whose assertion is not true **MUST** be refused `compensation-evidence-incomplete`. Empty lists
are valid. Lifecycle validates structure only and **MUST NOT** reconcile the lists against
Subscriptions. The same evidence guard applies to every `/workflow-cancel`, before the spawn
signal as after it (D-134).

**The failure reason is a closed enumeration** (D-136). A failed acknowledgement **MUST** carry
`failure_reason`, exactly one of:

| Value | Raised when |
|-------|-------------|
| `market-divergence` | the activation re-check returned `reject` for a line's market (`03 §3.6`) |
| `overlap-collision` | the re-check returned `reject` for an overlap collision, or Subscriptions raised one after the re-check (§4.3) |
| `identity-party-unavailable` | a re-check `defer` on the identity port (whose unavailable reason is `identity-party-unavailable`) exhausted `activation-recheck-retry-budget` (D-127) |
| `overlap-presence-unevaluable` | a re-check `defer` on the overlap-occupancy port exhausted the same budget (D-127) |
| `line-execution-failed` | a line's provisioning failed and Workflow's remediation was exhausted or its fail-fast policy applied |
| `dependency-graph-invalid` | the plan's Catalog dependency graph was missing a dependency or cyclic, so Workflow halted before any subscription was created |

A failed acknowledgement without a failure reason **MUST** be refused `failure-reason-missing`. A
value outside the enumeration, and a failure reason supplied with a completed acknowledgement,
are rejected at boundary validation with `request-invalid` (`01 §4.7` *Validation flow at the
boundary*, D-142), before authorization and unaudited, and never reach the engine. The reason is
recorded on the committed audit entry as its `caller_reason`, not its registered `reason`
(`01 §3.7`, D-143), and carried in `OrderFulfillmentFailed` (`01 §4.4`). Lifecycle records it and
never interprets it; adding a value is a contract change to this table and the event schema.

A failed acknowledgement driven by the activation re-check carries, as its failure reason, either
a `reject` line reason (`market-divergence`, `overlap-collision`) or — after an exhausted `defer` —
the unavailable port's reason (`identity-party-unavailable`, `overlap-presence-unevaluable`). A
`not-dispatchable` re-check outcome **MUST NOT** produce an acknowledgement at all: the held,
terminal or superseded order is re-read, not failed (`03 §3.6`, D-127).

The order **MUST NOT** wait on a Billing credit note. Operational compensation and financial
reversal are different concerns with different owners, and coupling order state to the second
would leave orders non-terminal for reasons the order has no visibility into.

### 4.5 The per-line projection is not a state machine (normative)

The order SoR **MUST NOT** contain a per-line fulfillment state machine. The projection carries
three values — created, activated, failed — sourced from acknowledgements, exposed for operator
visibility because two-phase fulfillment makes execution two visible waves.

No guard **MAY** read it, no order state **MAY** derive from it, and it **MUST NOT** define a
per-line terminal. Order terminals remain **atomic and order-level**: lines in one order are one
commercial intent, and partial completion is not representable. The downstream
`TransitionRequest` status **MUST NOT** be mirrored into it, nor **MAY** subscription-level
maker-checker approval holds be reflected into order state.

### 4.6 The upstream asks this slice depends on

The following dependencies are declared in [`../UPSTREAM_REQS.md`](../UPSTREAM_REQS.md) rather than only in prose —
which is how the numbering forked in the first place. `SUB-O1`, the compensation cancel reason, is
the one materially cheaper now than later, because reason values ride event payloads that
downstream consumers key on. `SUB-O2` would make provenance bidirectional. **`SUB-O10`** is new
and raised by this design: `create` and the activation intent must accept an explicit start
instant, so a line deferred past its quoted date cannot be backdated
([`../DECISIONS.md`](../DECISIONS.md) D-56).
`SUB-O5` is the overlap-occupancy read the activation re-check needs (amended from a presence read, D-126). `SUB-O9` — **cited by the
sibling Workflow PRD but absent from the seam map, so unregistered upstream** — would carry the
correlation identifier onward so an end-to-end trace does not stop at this seam.

The register itself has **forked**: the Subscriptions seam map defines `SUB-O1` through `SUB-O6`
while the sibling Workflow PRD cites `SUB-O5` through `SUB-O9`, with `SUB-O6`
carrying different meanings on the two sides. Reconciling it is a prerequisite for agreeing any
of them, and it is a document diff rather than a dependency on code.

The sixth ask is `cpt-cf-bss-orders-lifecycle-upreq-workflow-amendment-verdict`: the sibling gear
must consume `OrderAmended`, obtain the new version's requirement verdict and reflect it onward
from `submitted`. The current Workflow PRD limits that action to `OrderSubmitted`, so its PRD
needs amendment; the Lifecycle seam does not assume the missing behavior exists.

The seventh ask is `cpt-cf-bss-orders-lifecycle-upreq-overlap-activation-atomicity`: Subscriptions
must re-evaluate `overlapScopeKey` atomically with committing `active`. Until it does, the §4.3
activation re-check is an early abort only, with no admission guarantee (D-89).

## 5. Traceability

- **PRD**: [`../PRD.md`](../PRD.md) — §6.1 atomic fulfillment and subscription linkage, §6.4 seam rules R1–R5
- **Gear design**: [`../DESIGN.md`](../DESIGN.md) — realises `cpt-cf-bss-orders-lifecycle-component-workflow-seam`
- **Engine**: [`01-foundation`](./01-foundation.md) — transition contract, version check, audit, spawn-signal column
- **Depends on**: [`05-preconditions`](./05-preconditions.md) for both begin-fulfillment guards; [`03-gate-and-pin`](./03-gate-and-pin.md) for the activation re-check contract (specified by 03, executed by Workflow)
- **Consumers**: [`08-read-and-authz`](./08-read-and-authz.md) serves the per-line projection; [`07-hold-and-expiry`](./07-hold-and-expiry.md) exempts `in_fulfillment` (and holds whose `pre_hold_state` is `in_fulfillment`) from expiry by state, and reads the spawn signal only by deferring to §3.6 *Evaluate Cancel From In-Fulfillment (shared guard)* from its ordinary `POST /cancel`
- **Sibling gear**: [`orders-workflow/docs/PRD.md`](../../../orders-workflow/docs/PRD.md)
- **Upstream asks**: `SUB-O1`, `SUB-O2`, `SUB-O5`, `SUB-O9`, `SUB-O10`, `…-upreq-workflow-amendment-verdict` and `…-upreq-overlap-activation-atomicity` — per §4.6; additional recheck and progress integration requirements are recorded in UPSTREAM_REQS §2.6
- **ADRs**: [`ADR/0001`](../ADR/0001-cpt-cf-bss-orders-lifecycle-adr-transition-through-engine.md) transition through the engine; [`ADR/0002`](../ADR/0002-cpt-cf-bss-orders-lifecycle-adr-slice-decomposition.md) the foundation-plus-seven-slices decomposition
