<!-- CONFLUENCE_TITLE: [BSS]: Orders Workflow — Process Definition (Slice 10) -->
<!-- Related: ../DESIGN.md, ../PRD.md, ./01-foundation.md, ./README.md | Owners: BSS Orders team -->

# DESIGN — Process Definition (Slice 10)

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
- [4. Definition Normative Rules](#4-definition-normative-rules)
  - [4.1 The fence](#41-the-fence)
  - [4.2 The validation hook contract](#42-the-validation-hook-contract)
  - [4.3 Pinning](#43-pinning)
  - [4.4 Signal semantics](#44-signal-semantics)
  - [4.5 The hold pattern, and Q-11](#45-the-hold-pattern-and-q-11)
  - [4.6 Protected operations are never inside a swallowing `catch`](#46-protected-operations-are-never-inside-a-swallowing-catch)
  - [4.7 What a definition change may and may not do](#47-what-a-definition-change-may-and-may-not-do)
- [5. Traceability](#5-traceability)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-design-process-definition`

## 1. Architecture Overview

### 1.1 Architectural Vision

This document specifies the **order process definition**: the versioned Serverless Workflow
document that sequences the step operations of slices 01 through 09. Numbering is historical —
`02` through `09` were written before the decision of
[`../ADR/0011`](../ADR/0011-cpt-cf-bss-orders-workflow-adr-flow-as-platform-definition.md) — and
this document is **first in build order** after the foundation, because every other slice's §3.6
is now a path *through* the definition specified here (`./README.md`).

**Why the flow is a definition.** The order process has two kinds of content that change at
different rates and are reviewed by different people. What a step *does* — reflect a verdict into
Lifecycle under seam rule R1, dispatch a draft-create intent under the idempotency key of ADR-0006,
walk the compensation subjects in reverse ordinal — is commercial and integration logic with a
gear-owned record behind it; it changes with an Orders release. How the steps are *ordered* —
whether re-authorisation happens before or after plan freeze, how long an approval gate waits
before escalating, whether a partial failure parks or compensates under a given policy — is
process shape; it changes when the business changes its mind, and it should not need a Rust
release to do so. The platform gear `serverless-runtime` exists for exactly that split: it
registers Functions and Workflows as versioned definitions
([DESIGN §1.1](../../../../serverless-runtime/docs/DESIGN.md#11-architectural-vision)), its Temporal
plugin interprets the CNCF Serverless Workflow DSL on durable execution primitives
([ADR-0004](../../../../serverless-runtime/docs/ADR/0004-cpt-cf-serverless-runtime-adr-temporal-workflow-engine.md)
*Decision Outcome*), and the plugin — not the host and not the caller — owns timers, retry,
checkpoints, replay and event subscription
([ADR-0005](../../../../serverless-runtime/docs/ADR/0005-cpt-cf-serverless-runtime-adr-thin-host.md)
*Consequences*; [DESIGN `DESIGN.md:632`](../../../../serverless-runtime/docs/DESIGN.md#workflow)).
Orders therefore provides the operations and the record ([`01`](./01-foundation.md)) and lets
the platform provide the sequencing. Adjusting the flow is publishing a new definition version
through the platform registry; changing what a step does is an Orders release.

**What crosses the boundary is references.** A definition task carries `correlationId`,
`orderId`, `orderVersion`, `resourceTenantId`, the platform `invocationId`, a `gateRef`,
`taskRef`, `lineRef` or `stepRef`, and small closed enums an operation returned. It never carries
a resolved total, an approver identity, a seller or payer tenant axis, a justification or a
downstream payload ([`../ADR/0013`](../ADR/0013-cpt-cf-bss-orders-workflow-adr-references-not-payloads.md)),
which is what keeps commercial data out of the platform's history and satisfies the PRD §15 Q-01
criteria — history isolation, retention independent of engine purge, the BSS/OSS boundary — by
construction rather than by configuration.

**Honesty disclosure.** `serverless-runtime` has **no code and no Temporal plugin today**: its
design and ADRs exist, its SDK crate, host crate and `plugins/temporal-plugin/` do not. The
definitions in §3.6 are therefore **documentation until the platform readiness gate passes** —
the gate `01 §3.8` names, at which the registered definition version resolves in the platform
function registry and the invocation API answers. Nothing in this design set depends on the gate
passing for its *record* to be right: the step operations and the gear-owned tables of `01`–`09`
are unchanged if the platform slips, and only the sequencing would fall back to code
(`owf_definition_binding.definition_source = code`,
[`01 §3.7`](./01-foundation.md#37-database-schemas--tables)). That fallback is a **stated
property of the decomposition, not a plan**: no code-sequencing design is written, and none will
be unless the gate is declared failed
([`../ADR/0001`](../ADR/0001-cpt-cf-bss-orders-workflow-adr-durable-execution-substrate.md)).

### 1.2 Architecture Drivers

#### Functional Drivers

| Requirement | Design Response |
|-------------|------------------|
| `cpt-cf-bss-orders-workflow-fr-owf-start-contract` | The definition starts on `OrderSubmitted` or `OrderAmended` through a platform event trigger (§3.3) and `listen`s for the other Lifecycle triggers inside the running invocation, correlated on `orderId`/`orderVersion` (§3.6 (a), (b), (e), (f)); every consumed trigger passes Orders' `admit-trigger` first, which keeps admission and supersession. |
| `cpt-cf-bss-orders-workflow-fr-owf-approval-request`, `…-fr-owf-approval-escalation`, `…-fr-owf-approval-decision` | Path (a): `obtain-verdict` → `reflect-verdict` → `open-gates` → a competing `fork` of the decision `listen`, the escalation `wait` and the hold arm; the park arm on an unobtainable verdict. |
| `cpt-cf-bss-orders-workflow-fr-owf-payment-auth`, `…-fr-owf-fulfillment-plan`, `…-fr-owf-provisioning-intent` | Path (b): `evaluate-payment-auth-eligibility` with a `listen` for `OrderAcceptanceRecorded`, `construct-and-freeze-plan`, `begin-fulfillment`, the two waves and the barrier as an explicitly re-evaluated conjunction of the expected-time `wait` and the all-creates condition. |
| `cpt-cf-bss-orders-workflow-fr-owf-retry`, `…-fr-owf-manual-task`, `…-fr-owf-compensation-execution` | Path (c): task retry policy on the dispatch calls, `try`/`catch` around them, `create-manual-task`, a `listen` for the resolution and either resume or `compensate-order` → `report-outcome`. |
| `cpt-cf-bss-orders-workflow-fr-owf-terminal-order-events` | Path (d) for an authorised cancel and the terminal-event `listen` arm of path (f): `authorize-cancel` / `terminate-on-terminal-event` → `run-cancellation-fence` → `compensate-order` → `report-outcome` → `terminate-instance`. |
| `cpt-cf-bss-orders-workflow-fr-owf-hold-resume` | Path (e): hold and resume as signal arms that cancel and re-arm the escalation `wait` with the remainder `apply-hold`/`apply-resume` return; the lifetime ceiling as a top-level competing `wait` that no hold cancels. |
| `cpt-cf-bss-orders-workflow-fr-owf-overdue-escalation` | The overdue window as a `wait` arm competing with the fulfillment path and cancelled only by the order's own terminal process event; on completion `raise-overdue-escalation` and nothing else. |
| `cpt-cf-bss-orders-workflow-fr-owf-process-state-nonauth` | Every effect is a step operation that writes Orders' record; the definition holds no state Orders does not also record, and carries references only. |

#### NFR Allocation

| NFR ID | NFR Summary | Allocated To | Design Response | Verification Approach |
|--------|-------------|--------------|-----------------|----------------------|
| `cpt-cf-bss-orders-workflow-nfr-owf-durability` | Zero in-flight workflows lost across restarts | Platform plugin (invocation history) + `01` envelope | The plugin resumes the invocation; every re-issued call is absorbed under the same key | Platform worker kill/restart with the canonical definitions; no duplicate effect, no lost step |
| `cpt-cf-bss-orders-workflow-nfr-owf-escalation-timer` | Per-gate window, default 72 h, ± 5 min | Definition `wait` (plugin durable timer) | The escalation `wait` is armed per gate position with the window `open-gates` returns; hold cancels it and resume re-arms the remainder | Timer-accuracy test across a plugin worker restart; hold/resume test asserting the remainder |
| `cpt-cf-bss-orders-workflow-nfr-owf-fulfillment-sla` | p95 ≤ 15 min from activation eligibility to terminal outcome | Definition task timeouts and retry policy; `05` admission | Wave-2 task timeout 3 min, retry budget nested inside it by validation (§2); the barrier releases on the first evaluation after both conjuncts hold | Load test over the canonical definition |
| `cpt-cf-bss-orders-workflow-nfr-owf-audit` | 100 % of transitions recorded independently of engine history | `01` audit writer | The definition performs no effect outside a step operation, so every transition with an Orders consequence is audited by construction | Definition validation rule (§2): no `run`, no `emit`, every `call` a registered operation |
| `cpt-cf-bss-orders-workflow-nfr-owf-retention` | Gear-owned record ≥ 400 days independent of engine purge | `01` tables | Platform history retention (`TenantRuntimePolicy`, [`DESIGN.md:735`](../../../../serverless-runtime/docs/DESIGN.md#tenantruntimepolicy)) is set independently and may be shorter; nothing Orders needs lives only there | Purge-independence test |

#### Key ADRs

| ADR ID | Decision Summary |
|--------|-----------------|
| `cpt-cf-bss-orders-workflow-adr-flow-as-platform-definition` | The flow is a versioned Serverless Workflow definition in `serverless-runtime`, executed by its Temporal plugin; Orders provides step operations and the record |
| `cpt-cf-bss-orders-workflow-adr-definition-versioning-and-protected-steps` | Versions are validated before publish (protected operations present and ordered, closed trigger set, bound nesting, reference-only schemas, no swallowing `catch`); instances are pinned; a bound version is never deleted |
| `cpt-cf-bss-orders-workflow-adr-references-not-payloads` | Task inputs and outputs are references and small enums only |
| `cpt-cf-serverless-runtime-adr-workflow-dsl` | The platform's DSL is the CNCF Serverless Workflow Specification v1.0.0 (12 task types, jq expressions, compensation by `try`/`raise` composition) |
| `cpt-cf-serverless-runtime-adr-temporal-workflow-engine` | The platform's workflow engine plugin interprets that DSL on Temporal; signals, timers and schedules are Temporal-native |
| `cpt-cf-serverless-runtime-adr-thin-host` | The host owns registry, tenant policy, REST façade and invocation index; the plugin owns invocation, scheduling and event triggers |

### 1.3 Architecture Layers

```text
Platform registry     POST/GET /api/serverless-runtime/v1/functions — the definition versions,
(host)                validated on publish by Orders' validation hook (§3.3)
       │
Platform plugin       interprets the bound version: do · call · listen · wait · switch · fork ·
(Temporal)            try/catch/raise · set — durable timers, retry policy, replay, correlation
       │  HTTP call tasks
       ▼
Orders step surface   POST /bss-orders-workflow/v1/steps/{operation}  (01 §3.3)
       │
       ▼
Orders record         owf_definition_binding pins the version per instance (01 §3.7)
```

| Layer | Responsibility | Technology |
|-------|---------------|------------|
| Definition | The canonical document of §3.6: one workflow, six paths, calling only registered operations | Serverless Workflow DSL 1.0.0, YAML, jq expressions |
| Registry and validation | Draft, validate, publish, list versions; the pre-publish validation hook Orders supplies | serverless-runtime Function Registry API ([`DESIGN.md:857`](../../../../serverless-runtime/docs/DESIGN.md#function-registry-api)); plugin registration-validation hook (`DESIGN.md:762`) |
| Execution | Invocation lifecycle, timers, retry, event correlation, signals | serverless-runtime Temporal plugin (no code today) |
| Orders | Step operations, the binding, the record | [`01`](./01-foundation.md)–[`09`](./09-read-and-authz.md) |

## 2. Principles & Constraints

### 2.1 Design Principles

#### The definition orders; it never acts

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-principle-definition-orders-never-acts`

Every effect of the order process is a step operation of `01`–`09` invoked by a `call` task.
The definition uses **no `run`** task (containers, scripts, shell, sub-workflows would be effects
outside Orders' record) and **no `emit`** task (Orders publishes its six process events through
its own producer inside operations, [`01 §4.7`](./01-foundation.md#47-one-event-per-committed-step-outcome-and-the-six-named-process-events-only)).
It never calls Orders Lifecycle, Subscriptions, Payments or the Generic Approval service
directly — seam rules R1–R5 bind the operations — and its jq expressions select and re-key
references; they never compute a business value ([`01 §4.14`](./01-foundation.md#414-determinism-discipline-what-is-computed-on-which-side-of-the-boundary)).

**ADRs**: `cpt-cf-bss-orders-workflow-adr-flow-as-platform-definition`

#### Protected steps are ordered, never omitted

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-principle-protected-steps`

An operation registered `protected` ([`01 §3.3`](./01-foundation.md#33-api-contracts)) **MUST**
appear on every path that reaches its stage, in the order constraints of §4 *The fence*; a
definition version that omits one, replaces one with a `composable` operation or a Function, or
wraps one in a `catch` that swallows its failure **MUST** be refused by the validation hook. A
`composable` operation may be omitted or re-positioned inside its stage. This is what lets the
flow change without an Orders release while the fence — admission, binding, verdict, plan freeze,
begin-fulfillment, the waves, the spawn signal, the cancellation fence, compensation, outcome
report, termination — stays whole.

**ADRs**: `cpt-cf-bss-orders-workflow-adr-definition-versioning-and-protected-steps`

#### References, not payloads

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-principle-definition-references-only`

The input and output of every `call` validate against the operation's registered GTS reference
schemas; a definition whose task `input`/`output`/`export` names a member outside them is refused
before publish, and a call carrying one is refused at the envelope. `$context` holds only what an
operation returned. The platform's timeline and stored task payloads therefore contain identifiers
and enums and nothing an auditor would need Orders' retention policy for.

**ADRs**: `cpt-cf-bss-orders-workflow-adr-references-not-payloads`

### 2.2 Constraints

#### The grammar subset

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-constraint-definition-grammar-subset`

The Serverless Workflow Specification v1.0.0 defines twelve task types
([serverless-runtime ADR-0003](../../../../serverless-runtime/docs/ADR/0003-cpt-cf-serverless-runtime-adr-workflow-dsl.md)
*Consequences*). Orders definitions **MUST** use only the following, with the meaning the
specification gives them; the mapping to the platform is what the plugin executes, per
[serverless-runtime DESIGN `DESIGN.md:632`](../../../../serverless-runtime/docs/DESIGN.md#workflow)
("step identification and retry scheduling, compensation orchestration, checkpointing and
suspend/resume, and event subscription … using its backend's native primitives"):

| Construct | Spec meaning | Use in Orders definitions | Platform mapping |
|-----------|--------------|---------------------------|------------------|
| `do` | An ordered list of named tasks | The path body; every stage is a `do` list | Sequential activities of the interpreted workflow |
| `call: http` | Perform an HTTP request (`method`, `endpoint`, `headers`, `body`); a non-2xx answer raises a *communication* error carrying the HTTP `status` | Every step operation: `POST /bss-orders-workflow/v1/steps/{operation}` with `Idempotency-Key` derived from task inputs. A `call` to a registered Function is permitted **only** where the operation is `composable` | The plugin's activity for outbound HTTP; the 4xx/5xx answer is the `$error` the `catch` sees |
| `listen` | Consume one or more events matching filters (`to.one`, `to.any`, `to.all`), each filter `with` event properties and `correlate`-d on expressions | The eight non-start Lifecycle triggers, the approval decision, Subscriptions confirmations and operator signals, correlated on `orderId` and `orderVersion` | Temporal signal / event subscription with event-driven continuation (ADR-0004 *Option A*, `DESIGN.md:630`) |
| `wait` | Pause for a duration | Escalation window, barrier polling interval, expected-fulfillment wait, overdue window, lifetime ceiling | Temporal durable timer; survives worker restart |
| `switch` | Evaluate cases in order; the first `when` that holds (or the default case) selects a `then` | Verdict, reflection, admission, gate state, policy, resolution branching | Deterministic branch inside the interpreted workflow |
| `fork` | Run branches concurrently; `compete: true` completes with the first branch to finish and cancels the others | The gate loop (decision × escalation × hold × cancel × amendment), the barrier (confirmation `listen` × poll `wait`), the overdue arm, the lifetime arm | Concurrent branches with cancellation of the losers |
| `try` / `catch` | Run tasks; on an error matching `catch.errors`, optionally `retry` under a policy, then run `catch.do` | Retry of transient step failures under `use.retries.transient`; the failure arm of path (c) | Temporal retry policy on the activity, then the catch branch |
| `raise` | Raise an error (`type`, `status`, `title`, `detail`) | Converting an operation's permanent outcome into a fault the enclosing `catch` routes | Interpreted error |
| `set` | Set data in the task's output | Recording which arm of a `fork` completed, re-keying references into `$context` (with `export.as`) | Workflow-local data |

Not used: `run` and `emit` (above), and `for` — the spec's `for` iterates **sequentially**, and
`fork` takes a **static** branch list, so the DSL has no per-line dynamic parallel fan-out. Wave
dispatch is therefore **one `call` per wave carrying the line set as references**, and the bounded
per-order parallelism is inside `dispatch-wave1-create` / `dispatch-wave2-activate` under the
admission controls of [`05`](./05-provisioning-intents.md); this is registered under Q-11 (§4).

**Retry policy** is declared once in `use.retries.transient` — `delay` 1 s, `backoff.exponential`,
`jitter` from 0 s to 30 s, `limit.attempt.count` 5 — and attached by `catch.retry` to the `try`
around each call whose operation is `retryable-on: transient`. The retry condition is
`$error.status` in {429, 503, 504, 409}: the envelope answers a transient failure, an open
breaker, `still-processing` and `idempotency-lease-expired` with those statuses
([`01 §3.3`](./01-foundation.md#the-step-operation-contract)). A 409 `idempotency-key-conflict`
is a caller defect the same-key retry cannot fix; it exhausts the budget bounded and lands in the
failure arm with its reason. Whether the plugin surfaces the Problem body's `error_code` on
`$error` so the two 409s can be told apart before the budget is spent is part of Q-11.

#### The closed trigger set

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-constraint-definition-listen-targets`

A `listen` filter `with.type` **MUST** be one of: the nine Orders Lifecycle state events
(`OrderSubmitted` and `OrderAmended` as the start triggers of `schedule.on`; `OrderApproved`,
`OrderAmended`, `OrderHeld`, `OrderResumed`, `OrderAcceptanceRecorded`, `OrderCancelled`,
`OrderExpired`, `OrderRejected` as `listen` targets), whose GTS identifiers are Lifecycle's
(`gts.cf.core.events.event.v1~cf.bss.orders.event.v1~cf.bss.orders.<name>.v1~`,
[Lifecycle `01 §4.4`](../../../orders-lifecycle/docs/design/01-foundation.md#44-events-audit-and-the-outbox-normative));
the approval decision event of [`03 §3.3`](./03-approval-execution.md#33-api-contracts); the
two Subscriptions outcome events of [`05 §3.3`](./05-provisioning-intents.md#33-api-contracts)
(`ProvisioningIntentConfirmed`, `ProvisioningIntentFailed`); Orders' own
`OrderFulfillmentCompleted` and `OrderFulfillmentAborted` (used only to cancel the overdue arm);
and the operator signal types of §3.3 — `cancel-requested` (08), `reauthorize-requested` (04),
`task-resolution-requested` (07) and `unpark-requested`. Every filter **MUST** `correlate` on
`orderId` and `orderVersion` against `$context`, except the `OrderAmended` `listen`, which
correlates on `orderId` only because the amended version is newer (`02 §4.7` item 8). Every
`listen` that consumes one of the nine Lifecycle triggers **MUST** be followed by `admit-trigger`
with `role: listen` before any consuming operation (`02 §4.7` item 1). Any other type is refused
before publish.

#### Validation before publish

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-constraint-definition-validation-rules`

A definition version **MUST** pass every rule below in the validation hook the platform registry
calls before publish (§3.3) **and** in the CI test that runs the same rules over the canonical
definitions of §3.6 in this repository:

1. [ ] - `p1` - Every `protected` operation of each path appears exactly where §4 *The fence* orders it; `settle-from-lookup` never appears - `inst-def-protected-present`
2. [ ] - `p1` - Every `call: http` targets `POST /bss-orders-workflow/v1/steps/{operation}` with `{operation}` a row of `owf_step_operation`; a `call` to a Function is allowed only where the corresponding operation is `composable` - `inst-def-call-targets`
3. [ ] - `p1` - Every `listen` filter type is in the closed set above and carries the two correlations - `inst-def-listen-targets`
4. [ ] - `p1` - Bounds nest: for every `call`, the operation's `deadline_ms` **<** the cumulative backoff of its retry policy **<** the task's `timeout` **<** the overdue `wait` **<** the lifetime `wait` - `inst-def-bounds-nest`
5. [ ] - `p1` - Every task `input`, `output`, `export` and every `body` member validates against the operation's registered reference schemas; no member outside them - `inst-def-references-only`
6. [ ] - `p1` - No `protected` operation is inside a `try` whose `catch` has no `raise` or `then` that leaves the stage — its failure **MUST** propagate to the path's failure arm - `inst-def-no-swallowing-catch`
7. [ ] - `p1` - No `run`, no `emit`, no `for`; `schedule.on` names exactly `OrderSubmitted` and `OrderAmended` — Lifecycle publishes no `OrderSubmitted` after an amendment ([Lifecycle `04 §4.3`](../../../orders-lifecycle/docs/design/04-versioning.md#43-re-approval-is-a-two-step-seam-interaction-normative), `02 §4.7` item 9) - `inst-def-grammar-subset`
8. [ ] - `p1` - Every branch of a competing `fork` ends by `set`-ting `arm` so the sibling `switch` can route; every `fork` is followed by a `switch` on `arm` - `inst-def-fork-routing`

#### Versioning, pinning, publish

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-constraint-definition-versioning`

Definition versions follow the platform's GTS-canonical semver for callables
([`DESIGN.md:614`](../../../../serverless-runtime/docs/DESIGN.md#versioning-model)). A **new
version** is required for any change to the `do` structure, a `wait` value, a retry policy, a
`switch` predicate or a `listen` target; there is no in-place edit of a published version. An
instance is **pinned** at `start-instance` to the version the invocation runs
([`01 §3.7` `owf_definition_binding`](./01-foundation.md#table-owf_definition_binding)) and runs
to termination on it; migration is out of scope (PRD §5.2); a version **MUST NOT** be archived or
deleted while a binding names it, and the platform registry's `archived`/`deleted` transitions
(`DESIGN.md:588`) are gated by that check in the validation hook. **Publish roles**: the
platform operator today, through the registry's publish operation under platform authorization;
a seller-scoped fragment role is registered as **Q-10** (`../DECISIONS.md`). **Audit of
publishes**: the registry is the platform's system of record for who published what and when;
Orders copies `published_by` onto each binding at start (D-61 minimisation) so its own record
answers the question for the instances it ran.

**What changes without an Orders release**: the order of `composable` steps within a stage, every
`wait` value inside the nesting rule, retry attempts and backoff, `switch` predicates over
returned enums, which policy branch a partial failure takes, whether re-authorisation is asked
before or after plan freeze. **What does not**: the operation set and their contracts, the
protected order of §4, the closed trigger set, the six process events, the reason catalogue, any
table, any seam call.

## 3. Technical Architecture

### 3.1 Domain Model

**Technology**: Serverless Workflow DSL 1.0.0 documents (YAML) held in the platform function
registry; GTS reference schemas from `01 §3.3` for every task boundary.

**Core Entities**:

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-entity-process-definition`

The registered Workflow callable `gts.cf.core.sless.workflow.v1~cf.bss.orders_workflow.order_process.v1~`
(derived from the platform's workflow base type, [`DESIGN.md:279`](../../../../serverless-runtime/docs/DESIGN.md#functions-and-workflows)):
its `implementation` is the declarative `workflow_spec` of §3.6 (`DESIGN.md:388`), its
`workflow_traits` declare it **async-only** (it suspends on `listen` and `wait`; a sync invocation
**MUST** be rejected, `DESIGN.md:653`) and declare **no function-level `on_failure`/`on_cancel`
handler** — compensation is a path through Orders' own operations (§3.6 (c), (d)), never a
platform-invoked function that would act outside the record. Its `schema.params` is the reference
tuple of the start event. One callable, many versions.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-entity-definition-version`

One published, immutable revision of the process definition: the document of §3.6 at a semver,
the validation result the hook returned, the publishing principal and instant (registry-held).
It is what an instance pins to and what the fence of §4 is checked against.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-entity-definition-task`

One named task of a version: a `call` bound to one registered operation with a derived
idempotency key, or a `listen`, `wait`, `switch`, `fork`, `try`, `raise` or `set` of §2.2. A
`call` task's identity (`$task.name`) plus the invocation id is the `attemptId` the operation
records until the platform supplies its own ([`01 §3.3` *Attempt identity*](./01-foundation.md#33-api-contracts)).

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-entity-process-signal`

An operator-originated instruction delivered to a running invocation and consumed by a `listen`
arm: `cancel-requested` (an authorised workflow-mediated cancel, [`09 §3.3`](./09-read-and-authz.md#33-api-contracts)),
`reauthorize-requested` (a payment re-authorisation, [`04`](./04-fulfillment-plan.md)),
`task-resolution-requested` (an operator's `retry`, `override` or `cancel` of a manual task,
[`07 §3.3`](./07-manual-tasks.md#33-api-contracts)) and `unpark-requested` (after a
lifetime-ceiling park; origin route pending, §3.6 (d)). Hold
and resume are **not** signals of this kind: `OrderHeld`/`OrderResumed` are Lifecycle events the
definition `listen`s for directly. A signal carries the reference tuple and the operator's
`taskRef` or `requestRef` only; the operator's identity stays in Orders' audit entry, written by
the operation the arm calls.

**Relationships**:
- `Process definition` → `Definition version`: one-to-many; versions are immutable.
- `Definition version` → `Definition task`: one-to-many; a task belongs to exactly one version.
- `Definition task (call)` → `Step operation` (`01 §3.1`): many-to-one by operation name.
- `Definition version` → `Process instance` (`01 §3.1`): one-to-many through `owf_definition_binding`; never many-to-one.

### 3.2 Component Model

```mermaid
graph TB
    AUTH[Definition author / platform operator]
    REG[serverless-runtime Function Registry<br/>/api/serverless-runtime/v1/functions]
    HOOK[Orders validation hook<br/>rules of §2.2]
    OPR[owf_step_operation<br/>01 §3.7]
    TRG[Event triggers: OrderSubmitted, OrderAmended → order_process]
    PLG[Temporal plugin: running invocation]
    STEPS[Orders step surface 01 §3.3]
    SIG[Orders control gateway 09 §3.3 → invocations/{id}:plugin-control]
    AUTH -->|register draft, validate, publish| REG
    REG -->|registration-validation hook| HOOK
    HOOK -->|reads protected list, deadlines, schemas| OPR
    TRG -->|starts invocation of the bound version| PLG
    PLG -->|call tasks| STEPS
    SIG -->|signals consumed by listen arms| PLG
```

#### Definition validation hook

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-component-definition-validation-hook`

##### Why this component exists

The platform validates a definition against the Serverless Workflow JSON Schema
([serverless-runtime ADR-0003](../../../../serverless-runtime/docs/ADR/0003-cpt-cf-serverless-runtime-adr-workflow-dsl.md)
*Consequences*) and against the plugin's own rules through its registration-validation hook
([`DESIGN.md:762`](../../../../serverless-runtime/docs/DESIGN.md#function-registry)); neither knows
which operations are protected, which triggers are closed, or what Orders' deadlines are. This
component supplies that knowledge at the one moment it matters — before a version can be
published.

##### Responsibility scope

Implementing the rules of §2.2 *Validation before publish* over a candidate version: walking the
`do` tree, resolving every `call` endpoint to an `owf_step_operation` row, checking the fence
order of §4, the closed `listen` set, the bound nesting against `deadline_ms`, the reference-only
schemas, the no-swallowing-`catch` rule and the grammar subset; returning the platform's
`ValidationError` shape with a location per issue (`DESIGN.md:489`); refusing an archive or
delete of a version an `owf_definition_binding` names. The same code runs as the CI test over the
canonical definitions in this repository.

##### Responsibility boundaries

It authors nothing and publishes nothing; it never changes a definition to make it pass. Whether
the registry calls a gear-supplied hook before publish, rather than only the plugin's, is not
stated in the serverless-runtime design and is an **upstream ask** (`UPSTREAM_REQS.md` §2.9); until it is
answered the CI test is the enforcing check and a publish outside CI is an unvalidated publish
this design does not permit.

##### Related components (by ID)

- `cpt-cf-bss-orders-workflow-component-operation-registry` — depends on
- `cpt-cf-bss-orders-workflow-component-handler-extension-boundary` — shares model with

#### Signal delivery

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-workflow-component-signal-delivery`

##### Why this component exists

An operator cancel or re-authorisation must reach the running invocation as an event a `listen`
arm consumes, so that the definition — not an out-of-band Orders code path — decides what the
process does next, and so that Orders records the decision through `authorize-cancel` or
`evaluate-payment-auth-eligibility` like any other step.

##### Responsibility scope

Translating an authorised control operation of [`09 §3.3`](./09-read-and-authz.md#33-api-contracts)
into a signal to the instance's `invocation_id` (`01 §3.7`), carrying the reference tuple and the
signal type of §3.3; recording the request in Orders before delivery so a signal the platform loses
is visible as an unanswered request; retrying delivery under the caller-side duplicate protocol.

##### Responsibility boundaries

It performs no business effect and never bypasses the definition: the generic `:control cancel`
action of the platform (`DESIGN.md:885`) is **never** used for an order cancel, because it would
cancel the invocation without running the cancellation fence. The exact plugin-control verb and
payload that deliver a Temporal signal are an **upstream ask** (§3.3).

##### Related components (by ID)

- `cpt-cf-bss-orders-workflow-component-step-executor` — depends on

### 3.3 API Contracts

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-interface-definition-surface`

- **Requirement**: `cpt-cf-bss-orders-workflow-fr-owf-start-contract`
- **Technology**: the serverless-runtime REST surface, **by reference**; Orders defines no
  endpoint of its own in this document. Every path below is
  [serverless-runtime DESIGN §3.3](../../../../serverless-runtime/docs/DESIGN.md#33-api-contracts).

**The platform surface Orders uses**:

| Purpose | Platform endpoint | Reference | Orders' use |
|---------|-------------------|-----------|-------------|
| Register draft, validate, publish, list versions, deprecate | `/api/serverless-runtime/v1/functions` (CRUD over Function and Workflow entities) | `DESIGN.md:857` | Publishing a definition version; the validation hook of §3.2 runs inside "validate" and "publish"; `owf_definition_binding` blocks archive/delete of a bound version |
| Start an invocation | `POST /api/serverless-runtime/v1/invocations` with `function_id`, `mode: async`, `params`, `Idempotency-Key` | `DESIGN.md:865`, `DESIGN.md:895`–`910` | Not called by Orders — the event trigger starts every invocation. An operator re-drive of a dead invocation is `…:control` `retry` keeping `invocation_id` (D-86), never a second start; until the platform confirms that property, the instance is unwound and the order re-submitted, which starts through the trigger again |
| Read invocation status | `GET /api/serverless-runtime/v1/invocations/{invocation_id}` | `DESIGN.md:867` | The `reconciliation-sweep` worker's no-live-invocation metric (`01 §3.8`) and the progress read (`09`) |
| Generic control | `POST …/invocations/{invocation_id}:control` (`cancel`, `suspend`, `resume`, `retry`, `replay`) | `DESIGN.md:868`, `DESIGN.md:883`–`889` | `retry` from `failed` by an operator re-drive only; `cancel` **never** for an order cancel (§3.2); `suspend`/`resume` **never** — hold is a definition arm, not a platform suspension |
| Plugin control (signals) | `POST …/invocations/{invocation_id}:plugin-control` | `DESIGN.md:869`, `DESIGN.md:893` | Delivery of `cancel-requested`, `reauthorize-requested`, `task-resolution-requested` and `unpark-requested` to the running invocation's `listen` arms |
| Event trigger binding | `/api/serverless-runtime/v1/event-triggers` (create, enable, disable, metrics) | `DESIGN.md:980`–`987` | Two trigger bindings to `order_process` — `OrderSubmitted`, filtered to `category = new_sale`, and `OrderAmended` (`02 §2.2`); their dead-letter handling is the platform's (`01 §4.8`) |
| Timeline (debug) | `GET …/invocations/{invocation_id}/timeline` | `DESIGN.md:1061` | Operator debugging only; never an Orders read path |

**Signals.** The operator-facing instructions map as follows:

| Instruction | Origin | Delivery | Definition arm | Operation that records it |
|-------------|--------|----------|----------------|---------------------------|
| hold | Lifecycle `OrderHeld` | Broker event, correlated `listen` | §3.6 (e) | `apply-hold` (08) |
| resume | Lifecycle `OrderResumed` | Broker event, correlated `listen` | §3.6 (e) | `apply-resume` (08) |
| cancel | Seller operator, `POST /bss-orders-workflow/v1/workflows/{orderId}/cancel` (`09 §3.3`) | `…:plugin-control` signal `cancel-requested` | §3.6 (d) | `authorize-cancel` (08) |
| re-authorise | Operator / Payments outcome (`04`); origin route pending (`09 §3.3`) | `…:plugin-control` signal `reauthorize-requested` | §3.6 (b) | `evaluate-payment-auth-eligibility` (04) |
| resolve a manual task | Fulfillment or Seller operator `retry` / `override` / `cancel` (`07 §3.3`) | `…:plugin-control` signal `task-resolution-requested` | §3.6 (c) | `resolve-manual-task` (07) |
| unpark after the lifetime ceiling | Origin route pending (`09 §3.3`) | `…:plugin-control` signal `unpark-requested` | §3.6 (d) | `unpark` (01) |

The platform states that the plugin "owns the verb set its backend supports" on
`:plugin-control` (`DESIGN.md:893`) and that Temporal signals are native (ADR-0004 *Option A*);
it does **not** state a verb or payload shape for delivering a named signal that a `listen` task
consumes. **The exact plugin-control payload is an upstream ask** (`../UPSTREAM_REQS.md` §2.9).
Until answered, the signal types are declared here as reference names —
`gts.cf.core.events.event.v1~cf.bss.orders_workflow.signal.v1~cf.bss.orders_workflow.cancel_requested.v1~`,
`…reauthorize_requested.v1~`, `…task_resolution_requested.v1~` and `…unpark_requested.v1~` — and
are **never published to the broker**: they are not among the six process events and carry no
`data` beyond the reference tuple, a `requestRef` and, for `task-resolution-requested`, the
`taskRef`.

### 3.4 Internal Dependencies

| Dependency Gear | Interface Used | Purpose |
|-------------------|----------------|----------|
| `serverless-runtime` | Function registry, invocation, event-trigger APIs (§3.3) | Publishing and executing the definition; **no code today** |
| `orders-workflow` (this gear, `01`–`09`) | `POST /bss-orders-workflow/v1/steps/{operation}` | Every `call` task |
| `event-broker` | Broker topics of Lifecycle, Generic Approval, Subscriptions and Orders' own process events | The `listen` targets, consumed by the plugin's event subscription |

**Dependency Rules** (per project conventions):
- No circular dependencies
- Always use sdk modules for inter-gear communication
- No cross-category sideways deps except through contracts
- Only integration/adapter gears talk to external systems
- `SecurityContext` must be propagated across all in-process calls

### 3.5 External Dependencies

None. The definition calls no dependency outside this gear; Lifecycle, Subscriptions, Payments
and the Generic Approval service are reached only from inside step operations (seam rules R1–R5,
`01 §3.5`).

### 3.6 Interactions & Sequences

The canonical definition is **one document**; the six paths below are fragments of its `do` tree,
shown separately for review. Conventions used throughout: `$context.stepsBase` is the internal
step surface base URL; `ref` is the reference tuple every body carries —
`{ correlationId, orderId, orderVersion, resourceTenantId, invocationId: $workflow.id,
attemptId: ($workflow.id + ":" + $task.name) }`; a `call` shown as
`step: <operation>` expands to the `call: http` form of the first fragment; a `set` shown in a
fragment is also exported into `$context` (`export.as: ${ $context + . }`), which the
specification requires to be written out and these fragments elide; a `try` shown with
`retry: transient` catches the statuses of the retry condition of §2.2; the word
**protected** marks operations the fence of §4 requires. Every `body` member is a reference or an
enum (§2.1). Values marked *Q-11* depend on the DSL accepting a runtime expression where shown.

**Two routing conventions keep the arms honest.** (1) Every branch of a competing `fork`
**only listens or waits** and then `set`s `arm` (and the references the arm needs); no step
operation runs inside a competing branch, so a losing branch is never cancelled halfway through an
operation, and the sibling `switch` routes the winner to the path that calls the operation. (2)
Every stage loop records its own name in `$context.stageLoop` on entry; the shared paths that
return to "the stage the arm left" — an admission that answers `absorbed-duplicate`,
`ignored-superseded` or `ignored-terminated`, a recorded hold outside the approval stage, a
resume, a denied cancel, a `terminate: false` — end in `returnToStage`, a `switch` on
`$context.stageLoop` whose cases are the loop tasks named below (`parkLoop`, `parkLoopEscalated`,
`gateLoop`, `outageArm`, `awaitEligibilityChange`, `barrierLoop`, `awaitResolution`,
`awaitCompensationResolution`, `awaitOperatorAfterPark`, `awaitResume`).

#### (a) Start and approval

**ID**: `cpt-cf-bss-orders-workflow-seq-def-start-and-approval`

**Use cases**: `cpt-cf-bss-orders-workflow-fr-owf-start-contract`, `cpt-cf-bss-orders-workflow-fr-owf-approval-request`, `cpt-cf-bss-orders-workflow-fr-owf-approval-escalation`, `cpt-cf-bss-orders-workflow-fr-owf-approval-decision`

**Actors**: `cpt-cf-bss-orders-workflow-actor-owf-orders-lifecycle`, `cpt-cf-bss-orders-workflow-actor-owf-generic-approval`

```yaml
document:
  dsl: '1.0.0'
  namespace: cf-bss-orders-workflow
  name: order-process
  version: '1.0.0'
use:
  retries:
    transient:
      delay: { seconds: 1 }
      backoff: { exponential: {} }
      jitter: { from: { seconds: 0 }, to: { seconds: 30 } }
      limit: { attempt: { count: 5 } }
    supersession:                       # 02 §4.7 item 7: covers the prior version's pre-fulfillment unwind, nests below the lifetime ceiling
      delay: { minutes: 5 }
      backoff: { constant: {} }
      limit: { duration: { hours: 24 } }
    recheck:                            # 04 §4.8 item 4: at least 3 attempts within 60 s, so the operation's defer ladder, not the budget, decides
      delay: { seconds: 10 }
      backoff: { constant: {} }
      limit: { attempt: { count: 5 }, duration: { seconds: 60 } }
schedule:
  on:
    any:                                # the start bindings are exactly { OrderSubmitted, OrderAmended } (02 §4.7 item 9)
      - with: { type: gts.cf.core.events.event.v1~cf.bss.orders.event.v1~cf.bss.orders.submitted.v1~ }
      - with: { type: gts.cf.core.events.event.v1~cf.bss.orders.event.v1~cf.bss.orders.amended.v1~ }
input:
  from: >-
    ${ { orderId: .data.orderId, orderVersion: .data.orderVersion,
         resourceTenantId: .data.resourceTenantId, triggerEventId: .id,
         triggerKind: (if (.type | endswith("amended.v1~")) then "OrderAmended" else "OrderSubmitted" end) } }
do:
  - admitTrigger:                       # protected (02); role: start
      try:
        - admitStart:
            try:
              - call:
                  call: http
                  with:
                    method: post
                    endpoint: ${ $context.stepsBase + "/admit-trigger" }
                    headers:
                      Idempotency-Key: ${ .resourceTenantId + ":" + .triggerEventId + ":admit-trigger" }
                    body:
                      triggerEventId: ${ .triggerEventId }
                      triggerKind: ${ .triggerKind }
                      role: start
                      orderId: ${ .orderId }
                      orderVersion: ${ .orderVersion }
                      resourceTenantId: ${ .resourceTenantId }
                      invocationId: ${ $workflow.id }
                      attemptId: ${ $workflow.id + ":" + $task.name }
            catch: { errors: { with: { status: 503 } }, retry: transient }   # trigger-applicability-unverified, breaker, timeouts
      catch: { errors: { with: { status: 409 } }, retry: supersession }      # prior-instance-active (key left open); exhaustion fails the invocation (02 §4.7 item 6)
      export:
        as: '${ $context + { correlationId: .correlationId, admission: .admission } }'
  - onAdmission:                        # 02 §4.7 item 2: only `start` reaches start-instance
      switch:
        - start: { when: '${ $context.admission == "start" }', then: startInstance }
        - other: { then: end }          # absorbed-duplicate | ignored-superseded | ignored-terminated | no-active-instance
  - startInstance:                      # protected (01)
      step: start-instance              # body: ref + definitionId, definitionVersion: $workflow.definition.version, definitionSource: platform, triggerEventId
      export: { as: '${ $context + { rowVersion: .rowVersion, boundInvocationId: .invocationId } }' }
  - onBinding:
      switch:
        - notMine: { when: '${ $context.boundInvocationId != $workflow.id }', then: end }   # 01 §3.3: an existing binding answered; this invocation ends
        - mine:    { then: lifetime }
  - lifetime:
      fork:
        compete: true
        branches:
          - process:
              do:
                - approval: { do: [ obtainVerdict, onVerdict, reflectVerdict, afterReflect, openGates, gateLoop, afterGateLoop ] }   # this fragment
                - fulfillment: { do: [ ...fragment (b) ] }
                - arm: { set: { arm: process } }
          - lifetimeCeiling:            # top level, outside every stage fork: a hold never pauses it (08 §4.7 item 2)
              do:
                - waitCeiling: { wait: { days: 90 } }
                - arm: { set: { arm: lifetime } }
  - afterLifetime:
      switch:
        - ceiling: { when: '${ .arm == "lifetime" }', then: lifetimePark }
        - done:    { then: end }
  - lifetimePark:                       # 07 §4.8 item 7: the escalation before the park; park is permitted from started or suspended (01 §3.7)
      do:
        - escalate: { step: raise-overdue-escalation }     # body: ref + escalationKind: lifetime-ceiling; output: taskRef (the lifetime-ceiling-reached task)
        - parkCeiling: { step: park }                      # body: ref + parkReason: lifetime-ceiling, subjectRef: process
        - toOperator: { set: { stageLoop: awaitOperatorAfterPark }, then: awaitOperatorAfterPark }   # fragment (d)
```

The approval stage, inside `process.approval`:

```yaml
- obtainVerdict:                        # protected (03)
    try:
      - call: { step: obtain-verdict }  # output: verdict ∈ required | not-required | unobtainable; parkRef, parkReason on unobtainable
    catch: { errors: { with: { status: 503 } }, retry: transient }
    export: { as: '${ $context + { verdict: .verdict, parkRef: .parkRef, parkReason: .parkReason } }' }
- onVerdict:                            # 03 §4.5 item 2: unobtainable routes only to the park arm
    switch:
      - unobtainable: { when: '${ $context.verdict == "unobtainable" }', then: parkForVerdict }
      - obtained:     { then: reflectVerdict }
- parkForVerdict:                       # fail-closed park, ADR-0007 as amended; no hold arm in any park fork (03 §4.5 item 6)
    do:
      - park: { step: park }            # body: ref + parkReason: $context.parkReason, subjectRef: $context.parkRef
      - armEscalation:
          step: arm-park-escalation     # body: ref + parkRef; output: escalateAfter (null once the park has escalated)
          export: { as: '${ $context + { escalateAfter: .escalateAfter } }' }
      - onArm:
          switch:
            - escalated: { when: '${ $context.escalateAfter == null }', then: enterParkLoopEscalated }
            - armed:     { then: enterParkLoop }
      - enterParkLoop: { set: { stageLoop: parkLoop } }
      - parkLoop:
          fork:
            compete: true
            branches:
              - retryVerdict: { do: [ { waitRetry: { wait: { minutes: 5 } } }, { arm: { set: { arm: retry } } } ] }
              - escalation:   { do: [ { waitTtlMargin: { wait: '${ $context.escalateAfter }' } }, { arm: { set: { arm: escalate } } } ] }   # Q-11 (i)
              - lifecycle:    { do: [ ...fragment (f) lifecycle listen ] }        # sets arm: lifecycle
              - cancel:       { do: [ ...fragment (d) cancel listen ] }           # sets arm: cancel
      - afterParkLoop:
          switch:
            - retry:     { when: '${ .arm == "retry" }',     then: retryVerdict }
            - escalate:  { when: '${ .arm == "escalate" }',  then: escalatePark }
            - lifecycle: { when: '${ .arm == "lifecycle" }', then: admitLifecycle }   # fragment (f)
            - cancel:    { then: cancelPath }                                          # fragment (d)
      - escalatePark:                   # recorded once; the loop is re-entered WITHOUT the escalation branch (03 §4.5 item 6)
          step: raise-overdue-escalation   # body: ref + escalationKind: park, subjectRef: $context.parkRef
          then: enterParkLoopEscalated
      - enterParkLoopEscalated: { set: { stageLoop: parkLoopEscalated } }
      - parkLoopEscalated:
          fork:
            compete: true
            branches:
              - retryVerdict: { do: [ { waitRetry: { wait: { minutes: 5 } } }, { arm: { set: { arm: retry } } } ] }
              - lifecycle:    { do: [ ...fragment (f) lifecycle listen ] }
              - cancel:       { do: [ ...fragment (d) cancel listen ] }
      - afterParkLoopEscalated:
          switch:
            - retry:     { when: '${ .arm == "retry" }',     then: retryVerdict }
            - lifecycle: { when: '${ .arm == "lifecycle" }', then: admitLifecycle }
            - cancel:    { then: cancelPath }
      - retryVerdict:
          try:
            - call: { step: obtain-verdict }
          catch: { errors: { with: { status: 503 } }, retry: transient }
          export: { as: '${ $context + { verdict: .verdict } }' }
      - afterRetry:
          switch:
            - obtained: { when: '${ $context.verdict != "unobtainable" }', then: unparkForVerdict }   # unpark only after required | not-required
            - still:    { then: returnToStage }                                                   # parkLoop or parkLoopEscalated
      - unparkForVerdict: { step: unpark, then: reflectVerdict }    # body: ref + subjectRef: $context.parkRef
- reflectVerdict:                       # protected (03): reflects submitted → pending_approval | approved under R1
    try:
      - call: { step: reflect-verdict } # body: ref + stage: requirement
    catch:
      errors: { with: { status: 400 } } # a permanent refusal: approval-reflection-refused needs a human (03 §4.5 item 3)
      do: [ { toTask: { set: { failureScope: order, failureSubjects: [ '${ $context.correlationId }' ], failureReason: approval-reflection-refused, sourceStep: reflect-verdict }, then: createTasks } } ]   # fragment (c)
    export: { as: '${ $context + { reflected: .reflected } }' }   # reflected ∈ pending_approval | approved | rejected
- afterReflect:
    switch:
      - approved: { when: '${ $context.reflected == "approved" }', then: fulfillment }   # fragment (b)
      - rejected: { when: '${ $context.reflected == "rejected" }', then: terminateRejected }
      - pending:  { then: firstPosition }
- firstPosition: { set: { position: 0 } }   # 03 §4.5 item 7: 0 first, thereafter record-decision's nextPosition only
- openGates:                            # composable (03): opens every gate at the current sequence position
    step: open-gates                    # body: ref + position: $context.position; output: gateRefs[], position, escalationWindow, escalationRound
    export: { as: '${ $context + { gateRefs: .gateRefs, escalationRemaining: .escalationWindow, escalationRound: .escalationRound, probeRound: 0 } }' }
- enterGateLoop: { set: { stageLoop: gateLoop, holdPauses: true } }
- gateLoop:
    fork:
      compete: true
      branches:
        - decision:
            do:
              - awaitDecision:
                  listen:
                    to:
                      one:
                        with: { type: <approval decision event type, 03 §3.3> }
                        correlate:
                          orderId:      { from: '${ .data.orderId }',      expect: '${ $context.orderId }' }
                          orderVersion: { from: '${ .data.orderVersion }', expect: '${ $context.orderVersion }' }
              - arm: { set: { arm: decision, gateRef: '${ .data.gateId }', decisionEventId: '${ .data.decisionEventId }', outcome: '${ .data.outcome }' } }
        - escalation: { do: [ { waitEscalation: { wait: '${ $context.escalationRemaining }' } }, { arm: { set: { arm: escalate } } } ] }   # Q-11 (i); the value last returned by open-gates, escalate-gate or apply-resume
        - probe:      { do: [ { waitProbe: { wait: { seconds: 30 } } }, { arm: { set: { arm: probe } } } ] }   # 03 §4.5 item 5: ≤ 30 s
        - hold:       { do: [ ...fragment (e) hold listen ] }       # sets arm: held
        - resume:     { do: [ ...fragment (e) resume listen ] }     # sets arm: resumed (early resume, 08 §4.7 item 3)
        - lifecycle:  { do: [ ...fragment (f) lifecycle listen ] }  # sets arm: lifecycle
        - cancel:     { do: [ ...fragment (d) cancel listen ] }     # sets arm: cancel
- afterGateLoop:
    switch:
      - decision:  { when: '${ .arm == "decision" }',  then: recordDecision }
      - escalate:  { when: '${ .arm == "escalate" }',  then: escalateGate }
      - probe:     { when: '${ .arm == "probe" }',     then: probeGate }
      - held:      { when: '${ .arm == "held" }',      then: holdPath }        # fragment (e): the escalation wait was cancelled by winning
      - resumed:   { when: '${ .arm == "resumed" }',   then: earlyResumePath } # fragment (e)
      - lifecycle: { when: '${ .arm == "lifecycle" }', then: admitLifecycle }  # fragment (f)
      - cancel:    { then: cancelPath }                                         # fragment (d)
- recordDecision:                       # protected (03)
    step: record-decision               # body: ref + gateRef, decisionEventId, outcome ∈ approved | rejected
    export: { as: '${ $context + { gateState: .gateState, position: .nextPosition } }' }
- afterDecision:
    switch:
      - decided:      { when: '${ $context.gateState == "approved" or $context.gateState == "rejected" }', then: reflectGateOutcome }
      - nextPosition: { when: '${ $context.gateState == "next-position" }', then: openGates }
      - pending:      { then: gateLoop }
- reflectGateOutcome:                   # protected (03): pending_approval → approved | rejected; Lifecycle then emits OrderApproved | OrderRejected
    step: reflect-verdict               # body: ref + stage: gate-outcome; a permanent refusal takes the reflectVerdict catch above
    export: { as: '${ $context + { reflected: .reflected } }' }
    then: afterReflect
- escalateGate:                         # composable (03); enqueues OrderApprovalEscalated inside the operation
    step: escalate-gate                 # body: ref + position, mode: fire, round: $context.escalationRound
    export: { as: '${ $context + { escalationRemaining: .escalationRemaining, escalationRound: .escalationRound } }' }
    then: gateLoop
- probeGate:
    step: escalate-gate                 # body: ref + position, mode: probe, round: $context.probeRound
    export: { as: '${ $context + { serviceState: .serviceState, outageThresholdRemaining: .outageThresholdRemaining, probeRound: .probeRound, escalationRemaining: (.escalationRemaining // $context.escalationRemaining) } }' }
- afterProbe:
    switch:
      - outage:    { when: '${ $context.serviceState == "outage" }', then: enterOutageArm }
      - available: { then: gateLoop }
- enterOutageArm: { set: { stageLoop: outageArm, holdPauses: true } }
- outageArm:                            # 03 §4.5 item 5: probe loop until available, the outage threshold, hold and cancel
    fork:
      compete: true
      branches:
        - probe:     { do: [ { waitProbe: { wait: { seconds: 30 } } }, { arm: { set: { arm: probe } } } ] }
        - threshold: { do: [ { waitThreshold: { wait: '${ $context.outageThresholdRemaining }' } }, { arm: { set: { arm: outage-escalate } } } ] }   # Q-11 (i)
        - hold:      { do: [ ...fragment (e) hold listen ] }
        - resume:    { do: [ ...fragment (e) resume listen ] }
        - lifecycle: { do: [ ...fragment (f) lifecycle listen ] }
        - cancel:    { do: [ ...fragment (d) cancel listen ] }
- afterOutageArm:
    switch:
      - probe:     { when: '${ .arm == "probe" }',           then: probeOutage }
      - escalate:  { when: '${ .arm == "outage-escalate" }', then: escalateOutage }
      - held:      { when: '${ .arm == "held" }',            then: holdPath }
      - resumed:   { when: '${ .arm == "resumed" }',         then: earlyResumePath }
      - lifecycle: { when: '${ .arm == "lifecycle" }',       then: admitLifecycle }
      - cancel:    { then: cancelPath }
- probeOutage:
    step: escalate-gate                 # body: ref + position, mode: probe, round: $context.probeRound
    export: { as: '${ $context + { serviceState: .serviceState, probeRound: .probeRound, escalationRemaining: (.escalationRemaining // $context.escalationRemaining) } }' }
- afterProbeOutage:
    switch:
      - available: { when: '${ $context.serviceState == "available" }', then: enterGateLoop }   # re-enters gateLoop with the returned escalationRemaining
      - still:     { then: outageArm }
- escalateOutage:                       # recorded once per outage; the threshold is not re-armed
    step: raise-overdue-escalation      # body: ref + escalationKind: approval-outage, subjectRef: $context.position
    export: { as: '${ $context + { outageThresholdRemaining: { days: 90 } } }' }   # the threshold branch now never wins; the probe, hold, resume, lifecycle and cancel arms remain
    then: outageArm
- terminateRejected: { step: terminate-instance, then: end }   # protected (01); terminationKind: rejected
```

**Description**: The fragment reproduces [`02 §3.6`](./02-triggers-and-start.md#36-interactions--sequences)
*Start on trigger* and *Duplicate absorption* and [`03 §3.6`](./03-approval-execution.md#36-interactions--sequences)
*Verdict retrieval and reflection*, *Multi-party gate open* and *Decision reflection*, with the
timer and the pause moved into the definition. **Start.** The invocation starts on either start
binding (`OrderSubmitted` for a first version, `OrderAmended` for an amended one — Lifecycle
publishes no `OrderSubmitted` after an amendment, [Lifecycle `04 §4.3`](../../../orders-lifecycle/docs/design/04-versioning.md#43-re-approval-is-a-two-step-seam-interaction-normative));
only `admission = start` reaches `start-instance`, and every other admission ends the invocation
with no further call. A `prior-instance-active` refusal (409) is retried on the constant 5-minute
`supersession` policy for up to 24 h while the prior version's instance unwinds; the transient
policy handles the 503 family inside it. Because the plugin may not surface `error_code` on
`$error` (Q-11 (ii)), `still-processing` and `idempotency-lease-expired` also ride the
supersession cadence on this one call; the key stays `open` and the answer is the same.
**Approval.** The escalation window is the `wait` in the `escalation` branch; because the `fork`
competes and every branch only listens or waits, a decision, a hold, a probe or a cancel winning
the race cancels that `wait` — which is exactly the pause `03 §3.6` *Escalation timer fire and
approval-service outage pause* used to write into `owf_timer_pause`. Every re-entry arms the
duration the last operation returned (`open-gates`, `escalate-gate` or `apply-resume`), never a
literal, and `open-gates` receives `position = 0` first and `record-decision`'s `nextPosition`
thereafter; `escalate-gate` receives the `escalationRound` or `probeRound` it last returned. The
**approval-service outage** is observed by the `probe` branch (`escalate-gate` `mode: probe`); an
`outage` answer enters `outageArm`, which competes the probe loop, the outage threshold (followed
by one `raise-overdue-escalation` with `escalationKind: approval-outage`), the hold, the lifecycle
and the cancel arms, and an `available` answer re-enters `gateLoop` with the returned
`escalationRemaining`. **Park.** The park loop carries the verdict retry, the park escalation,
the lifecycle and the cancel arms and no hold arm; once `raise-overdue-escalation` has recorded the
park escalation — or `arm-park-escalation` answers a null `escalateAfter` on re-entry — the loop
continues as `parkLoopEscalated`, which has no escalation branch, so the park clock is never
re-armed. A repeat `obtain-verdict` for a version whose verdict is already reflected returns the
cached answer inside the operation and never re-reflects, as `03` requires; the definition does
not need to know. A permanent refusal of `reflect-verdict` (`approval-reflection-refused`) goes to
an order-scope manual task (fragment (c)), whose waiter carries the lifecycle and cancel arms. The
approved path ends at `reflect-verdict`; Lifecycle emits `OrderApproved`, and the fulfillment
stage is entered from `afterReflect` in the same invocation rather than from a second trigger. The
lifetime ceiling is the top-level competing arm: when it fires, `raise-overdue-escalation`
(`lifetime-ceiling`, which also opens the `lifetime-ceiling-reached` order task) precedes `park`,
from `started` or from `suspended` (`01 §3.7`), and the instance waits in
`awaitOperatorAfterPark`.

#### (b) Fulfillment: eligibility, plan, two waves and the barrier

**ID**: `cpt-cf-bss-orders-workflow-seq-def-fulfillment`

**Use cases**: `cpt-cf-bss-orders-workflow-fr-owf-payment-auth`, `cpt-cf-bss-orders-workflow-fr-owf-fulfillment-plan`, `cpt-cf-bss-orders-workflow-fr-owf-provisioning-intent`, `cpt-cf-bss-orders-workflow-fr-owf-line-progress`, `cpt-cf-bss-orders-workflow-fr-owf-overdue-escalation`

**Actors**: `cpt-cf-bss-orders-workflow-actor-owf-orders-lifecycle`, `cpt-cf-bss-orders-workflow-actor-owf-payments`, `cpt-cf-bss-orders-workflow-actor-owf-subscriptions`

```yaml
- fulfillment:
    do:
      - initEligibility: { set: { eligibilityTrigger: initial, evaluationSeq: 0, barrierSeq: 0, recheckSeq: 0, wave1Round: 0, wave2Round: 0, sweepRound: 0, planAttempt: 0, wave1Failed: [], wave2Failed: [] } }
      - eligibility:                                # protected (04)
          try:
            - call: { step: evaluate-payment-auth-eligibility }   # body: ref + trigger: $context.eligibilityTrigger, requestRef: $context.requestRef, evaluationSeq: $context.evaluationSeq
          catch: { errors: { with: { status: 503 } }, retry: transient }
          export: { as: '${ $context + { eligibility: .eligibility, eligibilitySeq: (if .eligibility == "eligible" then $context.evaluationSeq else $context.eligibilitySeq end), evaluationSeq: .nextEvaluationSeq } }' }
      - onEligibility:
          switch:
            - eligible: { when: '${ $context.eligibility == "eligible" }', then: freezePlan }
            - waiting:  { then: enterEligibilityWait }   # pending | withheld: settled successes that select the wait; the order stays approved
      - enterEligibilityWait: { set: { stageLoop: awaitEligibilityChange, holdPauses: false } }
      - awaitEligibilityChange:
          fork:
            compete: true
            branches:
              - acceptance:
                  do:
                    - listenAcceptance:
                        listen:
                          to:
                            one:
                              with: { type: gts.cf.core.events.event.v1~cf.bss.orders.event.v1~cf.bss.orders.acceptance_recorded.v1~ }
                              correlate:
                                orderId:      { from: '${ .data.orderId }',      expect: '${ $context.orderId }' }
                                orderVersion: { from: '${ .data.orderVersion }', expect: '${ $context.orderVersion }' }
                    - arm: { set: { arm: acceptance, lifecycleEventId: '${ .id }', triggerKind: OrderAcceptanceRecorded } }
              - reauthorize:
                  do:
                    - listenReauth: { listen: { to: { one: { with: { type: <reauthorize-requested signal, §3.3> }, correlate: { orderId: { from: '${ .data.orderId }', expect: '${ $context.orderId }' }, orderVersion: { from: '${ .data.orderVersion }', expect: '${ $context.orderVersion }' } } } } } }
                    - arm: { set: { arm: reevaluate, eligibilityTrigger: reauthorize-requested, requestRef: '${ .data.requestRef }' } }
              - poll:      { do: [ { waitPoll: { wait: { minutes: 5 } } }, { arm: { set: { arm: reevaluate, eligibilityTrigger: poll, requestRef: null } } } ] }   # 04 §4.8 item 5: bounded durable polling; covers an event delivered before the listen was armed
              - hold:      { do: [ ...fragment (e) hold listen ] }
              - resume:    { do: [ ...fragment (e) resume listen ] }
              - lifecycle: { do: [ ...fragment (f) lifecycle listen ] }
              - cancel:    { do: [ ...fragment (d) cancel listen ] }
      - afterEligibilityChange:
          switch:
            - acceptance: { when: '${ .arm == "acceptance" }', then: admitAcceptance }
            - again:      { when: '${ .arm == "reevaluate" }', then: eligibility }
            - held:       { when: '${ .arm == "held" }',       then: holdPath }
            - resumed:    { when: '${ .arm == "resumed" }',    then: earlyResumePath }
            - lifecycle:  { when: '${ .arm == "lifecycle" }',  then: admitLifecycle }
            - cancel:     { then: cancelPath }
      - admitAcceptance:                            # protected (02): admission before consumption (02 §4.7 item 1)
          step: admit-trigger                       # body: ref + triggerEventId: $context.lifecycleEventId, triggerKind, role: listen
          export: { as: '${ $context + { admission: .admission } }' }
      - onAcceptanceAdmission:
          switch:
            - advance:   { when: '${ $context.admission == "advance" }',   then: acceptanceRecorded }
            - supersede: { when: '${ $context.admission == "supersede" }', then: supersedePath }   # fragment (f)
            - terminate: { when: '${ $context.admission == "terminate" }', then: terminalEvent }   # fragment (f)
            - back:      { then: awaitEligibilityChange }
      - acceptanceRecorded: { set: { eligibilityTrigger: acceptance-recorded, requestRef: null }, then: eligibility }
      - freezePlan:                                 # protected (04)
          try:
            - call: { step: construct-and-freeze-plan }   # body: ref + attempt: $context.planAttempt; output: planRef, lineRefs[], expectedFulfillmentAt, policy, planState, reason
          catch: { errors: { with: { status: 503 } }, retry: transient }
          export: { as: '${ $context + { planRef: .planRef, lineRefs: .lineRefs, wave1LineRefs: .lineRefs, expectedFulfillmentAt: .expectedFulfillmentAt, policy: .policy, planState: .planState, planReason: .reason } }' }
      - onPlan:                                     # 04 §4.8 item 3 and §4.3
          switch:
            - frozen:           { when: '${ $context.planState == "frozen" }', then: beginFulfillment }
            - topology:         { when: '${ $context.planState == "topology-unavailable" }', then: planTask }       # a plan task under EITHER policy
            - invalidRemediate: { when: '${ $context.policy == "remediate" }', then: planTask }
            - invalidFailFast:  { then: planFailFast }
      - planTask: { set: { failureScope: plan, failureSubjects: [ '${ $context.planRef }' ], failureReason: '${ $context.planReason }', sourceStep: construct-and-freeze-plan, forceTask: true }, then: createTasks }   # fragment (c)
      - planFailFast:                               # no Workflow transition leaves approved: begin-fulfillment is passed before the unwind (04 §4.3)
          do:
            - passBegin: { step: begin-fulfillment }   # body: ref + planRef, eligibilitySeq
            - toUnwind: { set: { unwind: failure, reportAs: failed, terminationKind: compensated }, then: compensateOrder }   # fragment (c): nothing to void; fulfillment_failed with invalid-dependency-graph
      - beginFulfillment:                           # protected (04): the R1 seam call approved → in_fulfillment; enqueues OrderFulfillmentStarted
          try:
            - call: { step: begin-fulfillment }     # body: ref + planRef, eligibilitySeq: $context.eligibilitySeq; output: result ∈ in-fulfillment | withheld, withheldCause
          catch:
            errors: { with: { status: 409 } }       # version-mismatch from a concurrent amendment: wait for the amendment arm, never the failure path (02 §4.7 item 8)
            retry: transient
            do: [ { toWait: { then: enterEligibilityWait } } ]
          export: { as: '${ $context + { beginResult: .result } }' }
      - onBegin:                                    # 04 §4.8 item 2
          switch:
            - started:  { when: '${ $context.beginResult == "in-fulfillment" }', then: fulfillmentWithOverdue }
            - withheld: { then: enterEligibilityWait }   # back to the eligibility wait; the next eligible round carries a new eligibilitySeq
      - fulfillmentWithOverdue:
          fork:
            compete: true
            branches:
              - waves:
                  do:
                    - wave1:                              # protected (05): ONE call per wave carrying lineRefs[]; per-order parallelism and admission inside (05 §4.3)
                        try:
                          - dispatch:
                              try:
                                - call: { step: dispatch-wave1-create }   # body: ref + planRef, lineRefs: $context.wave1LineRefs, dispatchRound: $context.wave1Round, attemptKey: $context.attemptKey
                              catch: { errors: { with: { status: 503 } }, retry: transient, do: [ { toFailure: { set: { failureScope: line, failureSubjects: '${ $context.wave1LineRefs }', failureReason: wave1-create-failed, sourceStep: dispatch-wave1-create }, then: partialFailure } } ] }
                        catch:
                          errors: { with: { status: 409 } }   # still-processing | idempotency-lease-expired (Q-11 (ii)): read before re-issuing (05 §4.5 item 3)
                          do: [ { readFirst: { step: reconcile-intent } }, { reissue: { then: wave1 } } ]   # body: ref + sweepRound
                        export: { as: '${ $context + { wave1Failed: ($context.wave1Failed + .failed), wave1Deferred: .deferred, retryAfterMs: .retryAfterMs, wave1Round: .nextDispatchRound } }' }
                    - onWave1:
                        switch:
                          - deferred:  { when: '${ ($context.wave1Deferred | length) > 0 }', then: wave1Deferral }   # a settled success, never a failure (05 §4.5 item 4)
                          - anyFailed: { when: '${ ($context.wave1Failed | length) > 0 }', then: lineFailure1 }
                          - accepted:  { then: barrier }
                    - wave1Deferral: { do: [ { waitHint: { wait: '${ $context.retryAfterMs }' } }, { again: { set: { wave1LineRefs: '${ $context.wave1Deferred }' }, then: wave1 } } ] }   # Q-11 (i); a fixed 5 s until answered, the operation re-defers
                    - lineFailure1: { set: { failureScope: line, failureSubjects: '${ $context.wave1Failed }', sourceStep: dispatch-wave1-create }, then: partialFailure }   # fragment (c)
                    - barrier:                            # conjunction: expected time reached AND every create confirmed
                        do:
                          - waitExpected: { wait: '${ $context.expectedFulfillmentAt }' }   # Q-11 (i): duration to the instant; zero when already past
                          - enterBarrierLoop: { set: { stageLoop: barrierLoop, holdPauses: false } }
                          - barrierLoop:
                              do:
                                - evaluate:               # composable (04): reads Orders' record of confirmations; never the definition's memory
                                    step: evaluate-activation-eligibility   # body: ref + planRef, evaluationSeq: $context.barrierSeq
                                    export: { as: '${ $context + { released: .released, eligibleLineRefs: .eligibleLineRefs, barrierSeq: .nextEvaluationSeq } }' }
                                - onEvaluate:
                                    switch:
                                      - releasedFirst: { when: '${ $context.released and ($context.spawned | not) }', then: preActivation }
                                      - releasedAgain: { when: '${ $context.released }', then: wave2 }   # the spawn signal is recorded once
                                      - pending:       { then: awaitConfirmationOrPoll }
                                - awaitConfirmationOrPoll:     # re-evaluate on EVERY contributing signal: a confirmation OR the poll interval
                                    fork:
                                      compete: true
                                      branches:
                                        - confirmation:
                                            do:
                                              - listenConfirmation:
                                                  listen:
                                                    to:
                                                      any:
                                                        - with: { type: <ProvisioningIntentConfirmed, 05 §3.3> }
                                                          correlate: { orderId: { from: '${ .data.orderId }', expect: '${ $context.orderId }' }, orderVersion: { from: '${ .data.orderVersion }', expect: '${ $context.orderVersion }' } }
                                                        - with: { type: <ProvisioningIntentFailed, 05 §3.3> }
                                                          correlate: { orderId: { from: '${ .data.orderId }', expect: '${ $context.orderId }' }, orderVersion: { from: '${ .data.orderVersion }', expect: '${ $context.orderVersion }' } }
                                              - arm: { set: { arm: confirmation, confirmedLineRef: '${ .data.lineRef }', confirmedWave: '${ .data.wave }' } }   # 05 §4.5 item 7: the line reference and wave only
                                        - poll:      { do: [ { waitPoll: { wait: { seconds: 30 } } }, { arm: { set: { arm: poll } } } ] }
                                        - hold:      { do: [ ...fragment (e) hold listen ] }     # recorded, not waited on: holdPauses is false here
                                        - resume:    { do: [ ...fragment (e) resume listen ] }
                                        - lifecycle: { do: [ ...fragment (f) lifecycle listen ] }
                                        - cancel:    { do: [ ...fragment (d) cancel listen ] }
                                - afterSignal:
                                    switch:
                                      - confirmation: { when: '${ .arm == "confirmation" }', then: reconcileHint }
                                      - poll:         { when: '${ .arm == "poll" }',         then: reconcilePoll }
                                      - held:         { when: '${ .arm == "held" }',         then: holdPath }
                                      - resumed:      { when: '${ .arm == "resumed" }',      then: earlyResumePath }
                                      - lifecycle:    { when: '${ .arm == "lifecycle" }',    then: admitLifecycle }
                                      - cancel:       { then: cancelPath }
                                - reconcileHint:          # composable (05): the confirmation is a wake-up; read the line before re-evaluating
                                    step: reconcile-intent   # body: ref + lineRef: $context.confirmedLineRef, wave: $context.confirmedWave, sweepRound
                                    export: { as: '${ $context + { sweepFailed: .failed, sweepUnresolved: .unresolved, redispatch: .redispatch, sweepRound: .nextSweepRound } }' }
                                    then: onSweep
                                - reconcilePoll:          # composable (05): an early read of the due intents; settles a dead lease through settle-from-lookup in-process
                                    step: reconcile-intent   # body: ref + sweepRound
                                    export: { as: '${ $context + { sweepFailed: .failed, sweepUnresolved: .unresolved, redispatch: .redispatch, sweepRound: .nextSweepRound } }' }
                                - onSweep:                # 05 §4.5 item 6: the poll and confirmation arms never discard reconcile-intent's output
                                    switch:
                                      - failed:     { when: '${ (($context.sweepFailed + $context.sweepUnresolved) | length) > 0 }', then: sweepFailure }
                                      - redispatch1: { when: '${ [ $context.redispatch[] | select(.wave == "wave1_create") ] | length > 0 }', then: redispatchWave1 }
                                      - redispatch2: { when: '${ [ $context.redispatch[] | select(.wave == "wave2_activate") ] | length > 0 }', then: wave2 }
                                      - reevaluate: { then: evaluate }
                                - sweepFailure: { set: { failureScope: line, failureSubjects: '${ $context.sweepFailed + $context.sweepUnresolved }', sourceStep: reconcile-intent }, then: partialFailure }   # fragment (c): never-dispatched | intent-unresolved | wave failures
                                - redispatchWave1: { set: { wave1LineRefs: '${ [ $context.redispatch[] | select(.wave == "wave1_create") | .lineRef ] }' }, then: wave1 }
                    - preActivation:                      # protected (04): SUB-O5 overlap presence + market + authorization freshness, immediately before the first activation
                        try:
                          - call: { step: re-check-pre-activation }   # body: ref + planRef, evaluationSeq: $context.recheckSeq
                        catch: { errors: { with: { status: 503 } }, retry: recheck }
                        export: { as: '${ $context + { preActivation: .verdict, abortReason: .abortReason, recheckSeq: .nextEvaluationSeq } }' }
                    - onPreActivation:                    # 04 §4.8 item 4: proceed only by an explicit case
                        switch:
                          - proceed:         { when: '${ $context.preActivation == "proceed" }', then: spawnSignal }
                          - abort:           { when: '${ $context.preActivation == "abort" }', then: preActivationAbort }
                          - notDispatchable: { then: barrierLoop }   # the barrier's hold, lifecycle and cancel arms consume what the operation observed
                    - preActivationAbort: { set: { unwind: failure, reportAs: failed, terminationKind: compensated }, then: compensateOrder }   # never partialFailure; void wave-1 drafts, fulfillment_failed with abortReason
                    - spawnSignal:                        # protected (05): the first activation intent is the Lifecycle spawn/fencing signal
                        step: report-spawn-signal
                        export: { as: '${ $context + { spawned: true } }' }
                    - wave2:                              # protected (05): ONE call, eligible lines as references; the draft-liveness re-read is inside
                        try:
                          - dispatch:
                              try:
                                - call: { step: dispatch-wave2-activate }   # body: ref + planRef, lineRefs: $context.eligibleLineRefs, dispatchRound: $context.wave2Round, attemptKey
                              catch: { errors: { with: { status: 503 } }, retry: transient, do: [ { toFailure: { set: { failureScope: line, failureSubjects: '${ $context.eligibleLineRefs }', failureReason: wave2-activation-failed, sourceStep: dispatch-wave2-activate }, then: partialFailure } } ] }
                        catch:
                          errors: { with: { status: 409 } }   # activation-precondition-unmet, still-processing or idempotency-lease-expired: read, then back to the barrier
                          do: [ { readFirst: { step: reconcile-intent } }, { back: { then: barrierLoop } } ]
                        export: { as: '${ $context + { wave2Failed: ($context.wave2Failed + .failed), wave2Pending: .pending, lapsed: .lapsed, wave2Deferred: .deferred, retryAfterMs: .retryAfterMs, wave2Round: .nextDispatchRound } }' }
                    - onWave2:                            # 05 §4.5 items 4–6 and 9
                        switch:
                          - lapsed:     { when: '${ ($context.lapsed | length) > 0 }', then: rebuildLapsed }
                          - deferred:   { when: '${ ($context.wave2Deferred | length) > 0 }', then: wave2Deferral }
                          - anyFailed:  { when: '${ ($context.wave2Failed | length) > 0 }', then: lineFailure2 }   # D-54
                          - anyPending: { when: '${ ($context.wave2Pending | length) > 0 }', then: barrierLoop }  # dependents wait for their dependencies; the same conjunction
                          - complete:   { then: reportCompleted }                                                   # pending[] and failed[] both empty
                    - rebuildLapsed:                      # composable (05): a lapsed draft is rebuilt and goes back through wave 1 and the barrier, never straight to wave 2
                        step: rebuild-wave1               # body: ref + planRef, lineRefs: $context.lapsed, dispatchRound: $context.wave1Round
                        export: { as: '${ $context + { wave1LineRefs: .rebuilt, wave1Round: .nextDispatchRound } }' }
                        then: wave1
                    - wave2Deferral: { do: [ { waitHint: { wait: '${ $context.retryAfterMs }' } }, { again: { set: { eligibleLineRefs: '${ $context.wave2Deferred }' }, then: wave2 } } ] }
                    - lineFailure2: { set: { failureScope: line, failureSubjects: '${ $context.wave2Failed }', sourceStep: dispatch-wave2-activate }, then: partialFailure }
                    - reportCompleted:                    # protected (06): in_fulfillment → completed with per-line subscription ids; enqueues OrderFulfillmentCompleted
                        step: report-outcome              # body: ref + outcome: completed
                    - terminateCompleted: { step: terminate-instance }   # protected (01); terminationKind: completed
                    - arm: { set: { arm: done } }
              - overdue:                                  # the process deadline: 24 h past expected fulfillment time, raises an escalation and nothing else; no hold arm here
                  do:
                    - waitOverdue: { wait: '${ $context.expectedFulfillmentAt + 24h }' }   # Q-11 (i): duration to the instant
                    - overdueRace:
                        fork:
                          compete: true
                          branches:
                            - escalate:
                                do:
                                  - raise: { step: raise-overdue-escalation }   # composable (07); body: ref + escalationKind: overdue-fulfillment, stepRef
                                  - arm: { set: { arm: escalated } }
                            - terminal:                   # cancelled by the order's own terminal process event
                                do:
                                  - listenTerminal: { listen: { to: { any: [ { with: { type: gts.cf.core.events.event.v1~cf.bss.orders_workflow.event.v1~cf.bss.orders_workflow.fulfillment_completed.v1~ } }, { with: { type: gts.cf.core.events.event.v1~cf.bss.orders_workflow.event.v1~cf.bss.orders_workflow.fulfillment_aborted.v1~ } } ] } } }
                                  - arm: { set: { arm: terminal } }
                    - keepRunning: { wait: { days: 90 } }   # the overdue arm never wins the outer race; the waves branch or an unwind completes it
                    - arm: { set: { arm: overdue } }
      - afterFulfillment:
          switch:
            - done: { then: end }
```

**Description**: The fragment reproduces [`04 §3.6`](./04-fulfillment-plan.md#36-interactions--sequences)
*Plan Construction and Freeze*, *Pre-Activation Abort* and *Per-Line Progress*, and
[`05 §3.6`](./05-provisioning-intents.md#36-interactions--sequences) *Two-Wave Provisioning
Dispatch*, *Wave-1 Rebuild* and *Reconciliation Sweep Cycle*, in the order of
[`04 §4.8`](./04-fulfillment-plan.md#48-constraints-this-slice-places-on-the-definition) and
[`05 §4.5`](./05-provisioning-intents.md#45-constraints-this-slice-places-on-the-definition).
**Eligibility.** `evaluate-payment-auth-eligibility` is re-invoked from a competing fork that
carries the admitted `OrderAcceptanceRecorded` `listen`, the `reauthorize-requested` signal, a
poll `wait` and the hold, resume, lifecycle and cancel arms; `begin-fulfillment` follows only a
settled `eligible` and a settled `frozen`, receives the `eligible` round as `eligibilitySeq`, and a
`withheld` answer returns to the eligibility wait. Each of the three evaluation operations
returns `nextEvaluationSeq` and the definition passes it back unchanged. **Plan.** A `frozen` plan
proceeds; `topology-unavailable` opens a plan task under either policy; `invalid-graph` opens a
plan task under `remediate` and, under `fail-fast`, passes `begin-fulfillment` before the unwind
because no Workflow transition leaves `approved`. **Waves.** Each wave is **one `call` carrying
`lineRefs[]`**; per-line parallelism and admission are inside the operation (§2.2), and a wave's
per-line outcomes come back as reference lists that route the `switch`. A deferral (`deferred[]`
with `retryAfterMs`) is a settled success that waits and calls again under the next
`dispatchRound`; a `lapsed[]` line goes to `rebuild-wave1` and back through wave 1 and the
barrier; a `failed[]` line goes to fragment (c); a 409 is followed by a `reconcile-intent` read
before the call is re-issued. **The barrier is a conjunction and is re-evaluated on every
contributing signal**: `waitExpected` supplies the timer half once; `evaluate-activation-eligibility`
reads the all-creates half from Orders' record, which Orders' own consumer of the Subscriptions
outcome events maintains ([`09 §3.6` *System-actor delivery over the event broker*](./09-read-and-authz.md#36-interactions--sequences)),
so the definition never holds the confirmation count itself; and `awaitConfirmationOrPoll`
returns to `evaluate` on **either** a confirmation event **or** the poll interval — each through
`reconcile-intent`, whose `failed[]` and `unresolved[]` route to fragment (c) and whose
`redispatch[]` routes to the named wave — so a confirmation that landed between an evaluation and
the `listen` cannot hang the barrier and the timer fire is never the sole trigger
([`../ADR/0004`](../ADR/0004-cpt-cf-bss-orders-workflow-adr-two-wave-activation-barrier.md) as
amended). The poll arm is an early read of the schedule the `reconciliation-sweep` worker runs on
`next_sweep_at` for every instance (`01 §3.8`); a hold is recorded here and not waited on, so the
poll, `waitExpected` and the overdue `wait` keep running while the dispatch operations defer on
`owf_process_instance.suspended`. `re-check-pre-activation` runs once, before the first
activation: `proceed` is the only case that reaches `report-spawn-signal`, `abort` goes to the
unwind and `not-dispatchable` returns to the barrier loop. The overdue arm runs beside the waves
in a non-terminating position: it can escalate, but it never cancels fulfillment, which is why it
is inside its own `fork` and completes only against the order's terminal event
([`07`](./07-manual-tasks.md), `cpt-cf-bss-orders-workflow-fr-owf-overdue-escalation`).

#### (c) Partial failure: manual task, resume or compensate

**ID**: `cpt-cf-bss-orders-workflow-seq-def-partial-failure`

**Use cases**: `cpt-cf-bss-orders-workflow-fr-owf-retry`, `cpt-cf-bss-orders-workflow-fr-owf-manual-task`, `cpt-cf-bss-orders-workflow-fr-owf-override-semantics`, `cpt-cf-bss-orders-workflow-fr-owf-compensation-execution`

**Actors**: `cpt-cf-bss-orders-workflow-actor-owf-fulfillment-operator`, `cpt-cf-bss-orders-workflow-actor-owf-subscriptions`, `cpt-cf-bss-orders-workflow-actor-owf-orders-lifecycle`

```yaml
- partialFailure:                                   # entered with failureScope, failureSubjects (lineRef + reason, or a plan/order reference), sourceStep in $context
    do:
      - onPolicy:                                   # 07 §4.8 item 1
          switch:
            - remediate: { when: '${ $context.policy == "remediate" or $context.forceTask }', then: createTasks }
            - failFast:  { then: failFastUnwind }   # fail-fast records a tracked incident inside compensate-order's path; no actionable task
      - failFastUnwind: { set: { unwind: failure, reportAs: failed, terminationKind: compensated }, then: compensateOrder }
      - createTasks:                                # protected (07): exactly one actionable task per failed subject, reopen semantics inside
          step: create-manual-task                  # body: ref + scope: $context.failureScope, subjects: $context.failureSubjects, failureCause, sourceStep, sourceAttempt
          export: { as: '${ $context + { taskRefs: .taskRefs, exhaustedTaskRefs: .exhaustedTaskRefs, slaRemaining: .slaRemaining, slaRound: .slaRound, wave1Failed: [], wave2Failed: [], forceTask: false } }' }
      - onCreate:
          switch:
            - exhausted: { when: '${ ($context.exhaustedTaskRefs | length) > 0 }', then: failFastUnwind }   # the third failed attempt (07 §4.2)
            - open:      { then: enterAwaitResolution }
      - enterAwaitResolution: { set: { stageLoop: awaitResolution, holdPauses: false } }
      - awaitResolution:                            # 07 §4.8 item 3: every task has a waiter
          fork:
            compete: true
            branches:
              - resolution:
                  do:
                    - listenResolution: { listen: { to: { one: { with: { type: <task-resolution-requested signal, §3.3> }, correlate: { orderId: { from: '${ .data.orderId }', expect: '${ $context.orderId }' }, orderVersion: { from: '${ .data.orderVersion }', expect: '${ $context.orderVersion }' } } } } } }
                    - arm: { set: { arm: resolution, taskRef: '${ .data.taskRef }', requestRef: '${ .data.requestRef }' } }
              - sla:       { do: [ { waitSla: { wait: '${ $context.slaRemaining }' } }, { arm: { set: { arm: sla } } } ] }   # Q-11 (i)
              - hold:      { do: [ ...fragment (e) hold listen ] }
              - resume:    { do: [ ...fragment (e) resume listen ] }
              - lifecycle: { do: [ ...fragment (f) lifecycle listen ] }
              - cancel:    { do: [ ...fragment (d) cancel listen ] }
      - afterResolution:
          switch:
            - resolution: { when: '${ .arm == "resolution" }', then: resolveTask }
            - sla:        { when: '${ .arm == "sla" }',        then: slaCheck }
            - held:       { when: '${ .arm == "held" }',       then: holdPath }
            - resumed:    { when: '${ .arm == "resumed" }',    then: earlyResumePath }
            - lifecycle:  { when: '${ .arm == "lifecycle" }',  then: admitLifecycle }
            - cancel:     { then: cancelPath }
      - resolveTask:                                # composable (07)
          step: resolve-manual-task                 # body: ref + trigger: request, taskRef, requestRef
          export: { as: '${ $context + { resolution: .resolution, resumeAt: .resumeAt, attemptKey: .attemptKey, slaRemaining: .slaRemaining, slaRound: .slaRound } }' }
          then: onResolution
      - slaCheck:                                   # composable (07): an SLA breach escalates inside the operation
          step: resolve-manual-task                 # body: ref + trigger: sla-check, slaRound: $context.slaRound
          export: { as: '${ $context + { resolution: .resolution, slaRemaining: .slaRemaining, slaRound: .slaRound } }' }
      - onResolution:                               # 07 §4.8 item 4
          switch:
            - exhausted: { when: '${ $context.resolution == "exhausted" }', then: failFastUnwind }
            - override:  { when: '${ $context.resolution == "override" }',  then: verifyOverride }
            - retry:     { when: '${ $context.resolution == "retry" }',     then: routeRetry }
            - wait:      { then: returnToStage }    # closed | escalated | refused | none: back to the waiting fork
      - routeRetry:                                 # by resumeAt, carrying attemptKey
          switch:
            - barrier:      { when: '${ $context.resumeAt == "barrier" }',      then: barrier }          # the line re-enters the conjunction; the wave re-dispatches it under attemptKey
            - plan:         { when: '${ $context.resumeAt == "plan" }',         then: retryPlan }
            - compensation: { when: '${ $context.resumeAt == "compensation" }', then: compensate }       # a new pass of compensate-order
            - stage:        { then: retryStage }                                                        # an order-scope task: the stage whose operation failed
      - retryPlan: { set: { planAttempt: '${ $context.attemptKey }' }, then: freezePlan }   # a new attempt of construct-and-freeze-plan
      - retryStage:
          switch:
            - reflection: { when: '${ $context.sourceStep == "reflect-verdict" }', then: reflectVerdict }
            - cancel:     { when: '${ $context.sourceStep == "authorize-cancel" }', then: returnToStage }
            - admission:  { then: returnToStage }   # an unverified trigger on a listen arm: the arm's stage re-listens
      - verifyOverride:                             # composable (07): follows only a resolve-manual-task that answered override for the same requestRef
          step: verify-override                     # body: ref + taskRef, requestRef; output: verified, rejection, exhausted
          export: { as: '${ $context + { overrideVerified: .verified, overrideExhausted: .exhausted } }' }
      - afterOverride:
          switch:
            - verified:  { when: '${ $context.overrideVerified }', then: barrier }        # the line is activated; the conjunction decides whether the order completes
            - exhausted: { when: '${ $context.overrideExhausted }', then: failFastUnwind }
            - rejected:  { then: awaitResolution }                                       # the task remains open
- compensateOrder:                                  # the unwind (06 §4.7): fence < compensate-order < report-outcome < terminate-instance
    do:                                             # entered with unwind ∈ failure | cancel | supersede | terminal-event, reportAs and terminationKind in $context
      - fence:                                      # protected (06): claims or absorbs, promotes failure → cancel (06 §4.3)
          try:
            - call: { step: run-cancellation-fence }   # body: ref + trigger: $context.unwind, failureReason, cancelRequestRef, triggerEventId
          catch: { errors: { with: { status: 503 } }, retry: transient }   # a permanent failure raises into the path's failure arm with the returned reason
          export: { as: '${ $context + { pass: ($context.pass // 1) } }' }
      - compensate:                                 # protected (06): the whole reverse walk, ONE operation because the ordinal is Orders'
          try:
            - call: { step: compensate-order }      # body: ref + pass: $context.pass; output: compensationState ∈ complete | in-progress | pending-escalation, nextPass, taskRefs[]
          catch:
            errors: { with: { status: 503 } }
            retry: transient
            do: [ { nextPassAfterExhaustion: { set: { pass: '${ $context.pass + 1 }' }, then: enterAwaitCompensationResolution } } ]   # 06 §4.7 item 5
          export: { as: '${ $context + { compensationState: .compensationState, pass: .nextPass } }' }
      - onCompensation:                             # 06 §4.7 item 2: only complete reaches report-outcome
          switch:
            - complete:   { when: '${ $context.compensationState == "complete" }', then: reportOutcome }
            - inProgress: { when: '${ $context.compensationState == "in-progress" }', then: compensationRepoll }
            - pending:    { then: enterAwaitCompensationResolution }   # a leg failed: compensate-order already opened its manual task; the order stays non-terminal
      - compensationRepoll: { do: [ { waitRepoll: { wait: { seconds: 30 } } }, { again: { then: compensate } } ] }   # the barrier's poll interval; well inside the sweep floor
      - enterAwaitCompensationResolution: { set: { stageLoop: awaitCompensationResolution, holdPauses: false } }
      - slaOnEntry:                                 # 07 §4.8 item 3: the SLA branch calls sla-check on entry and waits the returned remainder
          step: resolve-manual-task                 # body: ref + trigger: sla-check, slaRound: $context.slaRound
          export: { as: '${ $context + { slaRemaining: .slaRemaining, slaRound: .slaRound } }' }
      - awaitCompensationResolution:                # no hold arm: a hold does not pause an unwind (06 §4.7 item 7)
          fork:
            compete: true
            branches:
              - resolution: { do: [ ...task-resolution-requested listen as in awaitResolution ] }   # sets arm: resolution, taskRef, requestRef
              - sla:        { do: [ { waitSla: { wait: '${ $context.slaRemaining }' } }, { arm: { set: { arm: sla } } } ] }
              - retryLeg:   { do: [ { waitRetry: { wait: { hours: 1 } } }, { arm: { set: { arm: retry } } } ] }   # inside the sweep floor
              - cancel:     { do: [ ...fragment (d) cancel listen ] }   # authorize-cancel, then the fence absorbs and promotes, then compensate
      - afterCompensationResolution:
          switch:
            - resolution: { when: '${ .arm == "resolution" }', then: resolveCompensationTask }
            - sla:        { when: '${ .arm == "sla" }',        then: slaOnEntry }
            - retry:      { when: '${ .arm == "retry" }',      then: compensate }    # the next pass; already-settled legs are absorbed
            - cancel:     { then: cancelPath }
      - resolveCompensationTask:
          step: resolve-manual-task                 # body: ref + trigger: request, taskRef, requestRef
          export: { as: '${ $context + { resolution: .resolution, attemptKey: .attemptKey } }' }
      - onCompensationResolution:
          switch:
            - again: { when: '${ $context.resolution == "retry" or $context.resolution == "exhausted" }', then: compensate }   # resumeAt: compensation
            - wait:  { then: slaOnEntry }                                                                                    # closed | escalated | refused | none
      - reportOutcome:                              # protected (06): the sole Lifecycle outcome caller
          try:
            - call: { step: report-outcome }        # body: ref + outcome: $context.reportAs ∈ failed | cancelled | superseded | terminal-event
          catch: { errors: { with: { status: 503 } }, retry: transient }   # outcome-not-reportable or fence-not-claimed raise into the path's failure arm
      - terminateAborted: { step: terminate-instance, then: end }   # protected (01); terminationKind: $context.terminationKind, supersededByOrderVersion on supersede
```

**Description**: The fragment reproduces [`06 §3.6`](./06-saga-and-compensation.md#36-interactions--sequences)
*Order-Level Failure Compensation* and *Compensation Walk Order*, and
[`07 §3.6`](./07-manual-tasks.md#36-interactions--sequences) *Manual Task Created on Failure* and
*Override Rejected Without Verified Subscription*. Retry exhaustion is the definition's — the
`catch.retry` on a wave call — and what follows it is `create-manual-task`, exactly as
[`08 §3.6` *Transient Outage: Retry-Then-Manual-Task*](./08-hold-and-cancel.md#36-interactions--sequences)
draws it; the envelope creates no task (`01 §4.5`). **Every task has a waiter**: `awaitResolution`
and `awaitCompensationResolution` compete the `task-resolution-requested` `listen` with an SLA
branch that waits the `slaRemaining` the last `resolve-manual-task` or `create-manual-task`
returned and then calls `resolve-manual-task` with `trigger: sla-check`. A non-empty
`exhaustedTaskRefs`, an `exhausted` resolution and an `exhausted: true` override route to the
unwind; a `retry` routes by `resumeAt` — `barrier` (a line), `plan` (a new
`construct-and-freeze-plan` attempt), `compensation` (a new `compensate-order` pass) or `stage` (an
order-scope task) — and carries `attemptKey`; `closed`, `escalated`, `refused` and `none` return
to the waiting fork. **The reverse walk is one operation**: `compensate-order` owns
`compensation_sequence`, the descending walk, the per-subject phase resolution and the
`failed-pending-escalation` records
([`../ADR/0005`](../ADR/0005-cpt-cf-bss-orders-workflow-adr-saga-compensable-no-pivot.md) as
amended), because splitting the walk into definition tasks would put the ordinal — a fact
Orders' saga log is authoritative for — into engine state. The definition keeps only `pass`, the
walk's resumption counter, and presents a greater `pass` on every re-invocation. `in-progress` is
re-polled on the barrier's interval; `pending-escalation` waits in `awaitCompensationResolution`,
which carries the cancel arm (a cancel during an unwind is authorized, then absorbed and promoted
by the fence, then the walk continues) and no hold arm. `report-outcome` is called only when
`compensate-order` reports `complete`, which is the PRD's "order remains non-terminal until
operational compensation reaches a known outcome".

#### (d) Cancel

**ID**: `cpt-cf-bss-orders-workflow-seq-def-cancel`

**Use cases**: `cpt-cf-bss-orders-workflow-fr-owf-compensation-execution`, `cpt-cf-bss-orders-workflow-fr-owf-terminal-order-events`

**Actors**: `cpt-cf-bss-orders-workflow-actor-owf-seller-operator`, `cpt-cf-bss-orders-workflow-actor-owf-orders-lifecycle`, `cpt-cf-bss-orders-workflow-actor-owf-subscriptions`

```yaml
# the cancel arm of every competing fork above: it only listens
- cancel:
    do:
      - awaitCancel:
          listen:
            to:
              one:
                with: { type: gts.cf.core.events.event.v1~cf.bss.orders_workflow.signal.v1~cf.bss.orders_workflow.cancel_requested.v1~ }
                correlate:
                  orderId:      { from: '${ .data.orderId }',      expect: '${ $context.orderId }' }
                  orderVersion: { from: '${ .data.orderVersion }', expect: '${ $context.orderVersion }' }
      - arm: { set: { arm: cancel, cancelRequestRef: '${ .data.requestRef }' } }
# the path the sibling switch routes to:
- cancelPath:
    do:
      - authorize:                                  # protected (08): the apply-time re-check of authority (09 §4.4); precedes the fence on every cancel path
          try:
            - call: { step: authorize-cancel }      # body: ref + cancelRequestRef; output: authorized (bool), taskRef
          catch:
            errors: { with: { status: 503 } }
            retry: transient
            do: [ { toTask: { set: { failureScope: order, failureSubjects: [ '${ $context.correlationId }' ], failureReason: authority-withdrawn, sourceStep: authorize-cancel }, then: createTasks } } ]   # 08 §4.7 item 6
          export: { as: '${ $context + { cancelAuthorized: .authorized } }' }
      - onAuthorize:
          switch:
            - authorized: { when: '${ $context.cancelAuthorized }', then: toCancelUnwind }
            - denied:     { then: returnToStage }   # the stage the cancel was taken from — the resume wait when taken from a hold (08 §4.7 item 5)
      - toCancelUnwind: { set: { unwind: cancel, reportAs: cancelled, terminationKind: compensated }, then: compensateOrder }   # fragment (c): workflow-mediated cancel with evidence
- awaitOperatorAfterPark:                           # after the lifetime ceiling parked the instance; no hold arm
    fork:
      compete: true
      branches:
        - cancel:    { do: [ ...cancel arm ] }
        - lifecycle: { do: [ ...fragment (f) lifecycle listen ] }
        - unpark:    { do: [ { listenUnpark: { listen: { to: { one: { with: { type: <unpark-requested signal, §3.3> }, correlate: { orderId: { from: '${ .data.orderId }', expect: '${ $context.orderId }' } } } } } } }, { arm: { set: { arm: unpark } } } ] }
- afterOperatorPark:
    switch:
      - cancel:    { when: '${ .arm == "cancel" }',    then: cancelPath }        # parked → compensating through the fence (01 §3.7)
      - lifecycle: { when: '${ .arm == "lifecycle" }', then: admitLifecycle }
      - unpark:    { then: unparkAfterCeiling }
- unparkAfterCeiling: { step: unpark, then: returnToStage }   # body: ref + subjectRef: process
```

**Description**: The fragment reproduces [`06 §3.6`](./06-saga-and-compensation.md#36-interactions--sequences)
*Authorized-Cancellation Compensation* and *Five-Step Cancellation Fencing* and
[`08 §3.6` *Workflow-Mediated Cancel with Compensation Evidence*](./08-hold-and-cancel.md#36-interactions--sequences).
The cancel is a signal (§3.3) because the platform's generic `cancel` would end the invocation
without the fence; the arm exists in every competing `fork` of the process so a cancel is
consumable in every stage, and the `switch` after each fork routes it to one `cancelPath`.
`authorize-cancel` is where the request's authority is re-checked at apply time — fencing can
outlive the request by days — and precedes `run-cancellation-fence` on every cancel path; a denied
re-check returns through `returnToStage` to the loop the cancel was taken from, which is the
resume wait (`awaitResume`) when the cancel was taken during a hold, because the instance is still
`suspended`. A retry exhaustion of `authorize-cancel` opens the `authority-withdrawn` task. A
parked instance — including one parked at the lifetime ceiling — reaches an unwind only through
this path and the fence (`parked → compensating`, `01 §3.7`). The terminal-event case
(`OrderCancelled`, `OrderExpired`, `OrderRejected`) is the same unwind entered through
`terminate-on-terminal-event` in fragment (f). `unpark-requested` has no origin route in this
design set yet ([`09 §3.3`](./09-read-and-authz.md#33-api-contracts); open question Q-13: the origin routes and catalogue pairs for `reauthorize-requested` and
`unpark-requested`, or their removal from the canonical definition).

#### (e) Hold and resume

**ID**: `cpt-cf-bss-orders-workflow-seq-def-hold-resume`

**Use cases**: `cpt-cf-bss-orders-workflow-fr-owf-hold-resume`

**Actors**: `cpt-cf-bss-orders-workflow-actor-owf-orders-lifecycle`, `cpt-cf-bss-orders-workflow-actor-owf-generic-approval`

```yaml
# the hold and resume arms of every stage fork (not the park loops, not the unwind): they only listen
- hold:
    do:
      - awaitHold:
          listen:
            to:
              one:
                with: { type: gts.cf.core.events.event.v1~cf.bss.orders.event.v1~cf.bss.orders.held.v1~ }
                correlate:
                  orderId:      { from: '${ .data.orderId }',      expect: '${ $context.orderId }' }
                  orderVersion: { from: '${ .data.orderVersion }', expect: '${ $context.orderVersion }' }
      - arm: { set: { arm: held, lifecycleEventId: '${ .id }', triggerKind: OrderHeld } }
- resume:                                           # the stage-level (early) resume arm: a resume delivered before its hold is not lost (08 §4.7 item 3)
    do:
      - awaitResume:
          listen:
            to:
              one:
                with: { type: gts.cf.core.events.event.v1~cf.bss.orders.event.v1~cf.bss.orders.resumed.v1~ }
                correlate:
                  orderId:      { from: '${ .data.orderId }',      expect: '${ $context.orderId }' }
                  orderVersion: { from: '${ .data.orderVersion }', expect: '${ $context.orderVersion }' }
      - arm: { set: { arm: resumed, lifecycleEventId: '${ .id }', resumeEventId: '${ .id }', triggerKind: OrderResumed, suspensionRef: null } }
# the path the sibling switch routes `held` to
- holdPath:
    do:
      - admitHold:                                  # protected (02): admission before consumption
          step: admit-trigger                       # body: ref + triggerEventId: $context.lifecycleEventId, triggerKind: OrderHeld, role: listen
          export: { as: '${ $context + { admission: .admission } }' }
      - onHoldAdmission:
          switch:
            - advance:   { when: '${ $context.admission == "advance" }',   then: applyHold }
            - supersede: { when: '${ $context.admission == "supersede" }', then: supersedePath }
            - terminate: { when: '${ $context.admission == "terminate" }', then: terminalEvent }
            - back:      { then: returnToStage }
      - applyHold:                                  # protected (08): phase → suspended; pauses the gate windows through slice 03's gate-window port
          step: apply-hold                          # body: ref + holdEventId: $context.lifecycleEventId, gateRefs: ($context.gateRefs // []); output: holdOutcome, suspensionRef, escalationRemaining
          export: { as: '${ $context + { holdOutcome: .holdOutcome, suspensionRef: .suspensionRef, escalationRemaining: (.escalationRemaining // $context.escalationRemaining) } }' }
      - onHold:
          switch:
            - pause:  { when: '${ $context.holdOutcome == "suspended" and $context.holdPauses }', then: enterResumeWait }   # only the approval stage waits on the resume
            - record: { then: returnToStage }       # suspended elsewhere, or reconciled-out-of-order | absorbed-duplicate | not-applicable: the stage continues, dispatch defers while suspended
      - enterResumeWait: { set: { heldStage: '${ $context.stageLoop }', stageLoop: awaitResume } }
      - awaitResume:
          fork:
            compete: true
            branches:
              - resume:
                  do:
                    - listenResume:
                        listen:
                          to:
                            one:
                              with: { type: gts.cf.core.events.event.v1~cf.bss.orders.event.v1~cf.bss.orders.resumed.v1~ }
                              correlate:
                                orderId:      { from: '${ .data.orderId }',      expect: '${ $context.orderId }' }
                                orderVersion: { from: '${ .data.orderVersion }', expect: '${ $context.orderVersion }' }
                    - arm: { set: { arm: resume, lifecycleEventId: '${ .id }', resumeEventId: '${ .id }', triggerKind: OrderResumed } }   # 08 §4.7 item 4: resumeEventId exported from the listen
              - lifecycle: { do: [ ...fragment (f) lifecycle listen ] }
              - cancel:    { do: [ ...fragment (d) cancel listen ] }   # a denied cancel returns here: stageLoop is awaitResume
      - afterResumeRace:
          switch:
            - resume:    { when: '${ .arm == "resume" }',    then: resumePath }
            - lifecycle: { when: '${ .arm == "lifecycle" }', then: admitLifecycle }
            - cancel:    { then: cancelPath }                    # an authorized cancel taken from hold: suspended → compensating
# the resume paths: from the resume wait (suspensionRef set) and from a stage-level resume arm (suspensionRef null)
- earlyResumePath: { set: { heldStage: '${ $context.stageLoop }' }, then: resumePath }
- resumePath:
    do:
      - admitResume:                                # protected (02)
          step: admit-trigger                       # body: ref + triggerEventId: $context.resumeEventId, triggerKind: OrderResumed, role: listen
          export: { as: '${ $context + { admission: .admission } }' }
      - onResumeAdmission:
          switch:
            - advance:   { when: '${ $context.admission == "advance" }',   then: applyResume }
            - supersede: { when: '${ $context.admission == "supersede" }', then: supersedePath }
            - terminate: { when: '${ $context.admission == "terminate" }', then: terminalEvent }
            - back:      { then: returnToStage }
      - applyResume:                                # protected (08): phase → started; re-arms the gate windows through the gate-window port
          step: apply-resume                        # body: ref + resumeEventId, suspensionRef (null on the stage-level arm); output: resumeOutcome, escalationRemaining, failedTaskRefs[]
          export: { as: '${ $context + { escalationRemaining: (.escalationRemaining // $context.escalationRemaining), resumeFailed: .failedTaskRefs, stageLoop: $context.heldStage } }' }
      - onResume:                                   # 08 §4.7 item 8: deferred failures first, before any dispatch
          switch:
            - deferredFailures: { when: '${ ($context.resumeFailed | length) > 0 }', then: resumeFailure }
            - back:             { then: returnToStage }   # the stage loop the hold interrupted, re-armed with escalationRemaining
      - resumeFailure: { set: { failureScope: line, failureSubjects: '${ $context.resumeFailed }', sourceStep: apply-resume }, then: partialFailure }   # fragment (c)
- returnToStage:                                    # routing convention (2) of §3.6
    switch:
      - parkLoop:                    { when: '${ $context.stageLoop == "parkLoop" }',                    then: parkLoop }
      - parkLoopEscalated:           { when: '${ $context.stageLoop == "parkLoopEscalated" }',           then: parkLoopEscalated }
      - gateLoop:                    { when: '${ $context.stageLoop == "gateLoop" }',                    then: gateLoop }
      - outageArm:                   { when: '${ $context.stageLoop == "outageArm" }',                   then: outageArm }
      - awaitEligibilityChange:      { when: '${ $context.stageLoop == "awaitEligibilityChange" }',      then: awaitEligibilityChange }
      - barrierLoop:                 { when: '${ $context.stageLoop == "barrierLoop" }',                 then: barrierLoop }
      - awaitResolution:             { when: '${ $context.stageLoop == "awaitResolution" }',             then: awaitResolution }
      - awaitCompensationResolution: { when: '${ $context.stageLoop == "awaitCompensationResolution" }', then: awaitCompensationResolution }
      - awaitOperatorAfterPark:      { when: '${ $context.stageLoop == "awaitOperatorAfterPark" }',      then: awaitOperatorAfterPark }
      - awaitResume:                 { then: awaitResume }
```

**Description**: The fragment reproduces [`08 §3.6` *Hold Then Resume with Remaining-Window
Timer Preservation*](./08-hold-and-cancel.md#36-interactions--sequences) as a definition pattern,
in the order of [`08 §4.7`](./08-hold-and-cancel.md#47-constraints-this-slice-places-on-the-definition).
**Only the approval-escalation wait pauses on hold**: the hold arm sits inside `gateLoop` (and
`outageArm`), and because it only listens, the hold winning the race cancels the `escalation`
branch's `wait` at once. `holdPath` admits the event, and `apply-hold` records the suspension and
pauses the gate windows through slice 03's gate-window port; in the approval stage
(`holdPauses`) the definition then waits in `awaitResume`, and after `apply-resume` the stage
loop is re-entered with the `escalationRemaining` `apply-resume` returned — the remainder the
gate-window port computed from `owf_approval_gate`, never the definition's own arithmetic and
never a value re-derived from the gate's `opened_at`. `apply-resume` does not re-read drafts: the
draft-liveness re-read is inside the wave-2 dispatch (slice 05). A non-empty `failedTaskRefs[]`
(failures slice 05 recorded as deferred while held) routes to fragment (c) before any dispatch.
**The lifetime ceiling, the barrier and the overdue window keep running**: the lifetime `wait` is
the top-level competing arm of fragment (a), outside every stage fork; outside the approval stage
a hold is recorded and the stage loop continues, so the barrier poll, `waitExpected` and the
overdue `waitOverdue` are never cancelled by a hold, and the dispatch operations defer while
`owf_process_instance.suspended` is set ([`01 §3.7`](./01-foundation.md#table-owf_process_instance));
the stage-level resume arm then records the resume. Every stage fork that carries a hold arm also
carries that resume arm, so a resume delivered before its hold is recorded as
`resume-ahead-recorded` and not lost, and the resume wait is entered only on
`holdOutcome = suspended`. A cancel taken from the resume wait that `authorize-cancel` denies
returns to the resume wait, because the instance is still `suspended`. The park loops and the
unwind carry no hold arm (`03 §4.5` item 6, `06 §4.7` item 7). Whether the DSL accepts the
remainder as a runtime-expression `wait` duration, and whether the pattern as a whole is
expressible natively, is **Q-11** (§4.5); if it is not, the re-arm is a registered Function that
sleeps the remainder, which is the one place a Function would enter these definitions.

#### (f) Amendment and terminal order events

**ID**: `cpt-cf-bss-orders-workflow-seq-def-amendment-and-terminal`

**Use cases**: `cpt-cf-bss-orders-workflow-fr-owf-start-contract`, `cpt-cf-bss-orders-workflow-fr-owf-terminal-order-events`, `cpt-cf-bss-orders-workflow-fr-owf-approval-request`

**Actors**: `cpt-cf-bss-orders-workflow-actor-owf-orders-lifecycle`

```yaml
# the lifecycle arm of every competing fork before and after begin-fulfillment: it only listens
- lifecycle:
    do:
      - awaitLifecycle:
          listen:
            to:
              any:
                - with: { type: gts.cf.core.events.event.v1~cf.bss.orders.event.v1~cf.bss.orders.amended.v1~ }
                  correlate: { orderId: { from: '${ .data.orderId }', expect: '${ $context.orderId }' } }   # a NEWER version: no orderVersion correlation (02 §4.7 item 8)
                - with: { type: gts.cf.core.events.event.v1~cf.bss.orders.event.v1~cf.bss.orders.cancelled.v1~ }
                  correlate: { orderId: { from: '${ .data.orderId }', expect: '${ $context.orderId }' }, orderVersion: { from: '${ .data.orderVersion }', expect: '${ $context.orderVersion }' } }
                - with: { type: gts.cf.core.events.event.v1~cf.bss.orders.event.v1~cf.bss.orders.expired.v1~ }
                  correlate: { orderId: { from: '${ .data.orderId }', expect: '${ $context.orderId }' }, orderVersion: { from: '${ .data.orderVersion }', expect: '${ $context.orderVersion }' } }
                - with: { type: gts.cf.core.events.event.v1~cf.bss.orders.event.v1~cf.bss.orders.rejected.v1~ }
                  correlate: { orderId: { from: '${ .data.orderId }', expect: '${ $context.orderId }' }, orderVersion: { from: '${ .data.orderVersion }', expect: '${ $context.orderVersion }' } }
      - arm: { set: { arm: lifecycle, lifecycleEventId: '${ .id }', triggerKind: '${ .type }', newOrderVersion: '${ .data.orderVersion }' } }   # triggerKind is mapped to the closed nine-value enum; nothing branches on it
# the paths the sibling switch routes to: admission first, and the admission alone decides (02 §4.7 items 1 and 3)
- admitLifecycle:                                   # protected (02)
    step: admit-trigger                             # body: ref + triggerEventId: $context.lifecycleEventId, triggerKind, role: listen; output: admission ∈ advance | supersede | terminate | absorbed-duplicate | ignored-superseded | ignored-terminated
    export: { as: '${ $context + { admission: .admission } }' }
- onLifecycleAdmission:
    switch:
      - supersede: { when: '${ $context.admission == "supersede" }', then: supersedePath }
      - terminate: { when: '${ $context.admission == "terminate" }', then: terminalEvent }
      - back:      { then: returnToStage }          # absorbed-duplicate (an OrderAmended at the pinned version) | ignored-superseded | ignored-terminated
- supersedePath: { set: { unwind: supersede, reportAs: superseded, terminationKind: superseded, triggerEventId: '${ $context.lifecycleEventId }' }, then: compensateOrder }   # fragment (c): fence (supersede) cancels open gates, voids un-activated wave-1 drafts, compensates activated; report-outcome makes no seam call
- terminalEvent:                                    # protected (02): OrderCancelled | OrderExpired | OrderRejected for an active instance
    step: terminate-on-terminal-event               # body: ref + triggerEventId: $context.lifecycleEventId; output: terminate (bool)
    export: { as: '${ $context + { terminate: .terminate } }' }
- onTerminal:
    switch:
      - unwind: { when: '${ $context.terminate }', then: terminalUnwind }
      - back:   { then: returnToStage }             # terminate: false returns to the arm's stage and never skips to terminate-instance (02 §4.7 item 4)
- terminalUnwind: { set: { unwind: terminal-event, reportAs: terminal-event, terminationKind: terminal-order-event, triggerEventId: '${ $context.lifecycleEventId }' }, then: compensateOrder }   # report-outcome makes no Lifecycle transition: the order is already terminal
```

**The new version's invocation.** `OrderAmended` has two consumers. The platform event trigger of
§3.3 binds **both** start triggers, `OrderSubmitted` and `OrderAmended`, because Lifecycle
publishes `OrderAmended`, not a second `OrderSubmitted`, for an amended order
([Lifecycle `04 §4.3`](../../../orders-lifecycle/docs/design/04-versioning.md#43-re-approval-is-a-two-step-seam-interaction-normative),
[`02 §2.2`](./02-triggers-and-start.md#22-constraints)); the amended version's invocation starts
through `admit-trigger` with `role: start` under the new `orderVersion`, and while the old
version's instance is still unwinding its admission answers `prior-instance-active` and the
`supersession` retry policy of fragment (a) waits it out. The old invocation's lifecycle arm
consumes the same event with `role: listen`, is admitted as `supersede` and unwinds through the
fence; `start-instance` for the new version succeeds once the old instance's `terminal_outcome`
is set (`01 §3.7` partial unique index). The two consumers are separated by the role suffix of
the admission key (`02 §2.1`), never by two deliveries to one consumer. Whether one broker event
can both start a new invocation through a trigger and be consumed by a running invocation's
`listen` is not stated in the serverless-runtime design and is an **upstream ask** (`UPSTREAM_REQS.md` §2.9).

**Description**: The fragment reproduces [`02 §3.6` *Terminal-event compensation, void, and
audit*](./02-triggers-and-start.md#36-interactions--sequences) and `02 §4`'s void-on-superseded-
version rule, in the order of
[`02 §4.7`](./02-triggers-and-start.md#47-constraints-this-slice-places-on-the-definition). Every
listened Lifecycle trigger — here, and `OrderAcceptanceRecorded`, `OrderHeld` and `OrderResumed`
in fragments (b) and (e) — passes `admit-trigger` with `role: listen` before any consuming
operation, and the returned `admission`, never event data, selects the path. Termination is
symmetric with start: a terminal order event and a superseding version both run the cancellation
fence and the compensation walk before `terminate-instance`, and neither leaves a wave-1 draft for
a platform TTL this gear does not own. The admission decision — is this event for a superseded,
current or newer version — stays in `admit-trigger`, read against Lifecycle under R1, never in a
jq comparison over event data.

### 3.7 Database schemas & tables

This document owns **no table**. Definition versions live in the platform function registry
(`functions`, [serverless-runtime DESIGN `DESIGN.md:1143`](../../../../serverless-runtime/docs/DESIGN.md#logical-tables));
the binding of an instance to a version is `owf_definition_binding`
([`01 §3.7`](./01-foundation.md#table-owf_definition_binding)); the operations a definition may
call are `owf_step_operation` ([`01 §3.7`](./01-foundation.md#table-owf_step_operation)). The
canonical definitions of §3.6 are also kept **in this repository** as YAML under the gear's
`definitions/` directory so the CI validation test of §2.2 runs against the same bytes that are
published; the repository copy is the review artifact, the registry copy is the executed one, and
the publish step asserts they are byte-identical.

### 3.8 Deployment Topology

- [ ] `p3` - **ID**: `cpt-cf-bss-orders-workflow-topology-process-definition`

The definition executes on the platform's Temporal plugin workers
([serverless-runtime DESIGN §1.4.4](../../../../serverless-runtime/docs/DESIGN.md#144-gear-lifecycle)),
which are a platform deployable, not an Orders one; Orders deploys no workflow worker. Orders'
side of the topology is: the step surface of `01 §3.3` reachable from the plugin's workers under
the service principal; the event trigger of §3.3 provisioned once per environment and enabled
only after the readiness gate; the validation hook of §3.2 registered with the platform registry
(or, until the hook exists on the platform side, the CI test as the gate); and the repository
`definitions/` directory published to the registry by the release pipeline, never by hand. One
definition version is **active for new instances** per environment at a time; older versions stay
published while bindings name them and are deprecated, never deleted, thereafter.

**Observability owned here**: active definition version per environment; count of instances
bound per version (`01 §3.8`); validation-hook refusals by rule; signal deliveries by type and
delivery outcome; the platform's invocation status distribution for `order_process` read from
`GET /api/serverless-runtime/v1/invocations` (`DESIGN.md:866`) — `suspended` is the normal state
of a healthy long-running instance; `failed` and `dead_lettered` are what the
`reconciliation-sweep` worker reports as intents with no live invocation (`01 §3.8`).

## 4. Definition Normative Rules

### 4.1 The fence

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-constraint-definition-fence`

Every published version **MUST** contain the protected operations of
[`01 §3.3`](./01-foundation.md#the-step-operation-contract) and the slices' §3.3 declarations in
the following order constraints, and the validation hook **MUST** refuse a version that violates
any of them:

| Stage | Protected operations, in order | May be interleaved with (composable) |
|-------|-------------------------------|--------------------------------------|
| Admission | `admit-trigger` (`role: start`) **<** `start-instance`; on every arm that consumes a Lifecycle trigger, `admit-trigger` (`role: listen`) **<** the consuming operation (`evaluate-payment-auth-eligibility`, `apply-hold`, `apply-resume`, `terminate-on-terminal-event`, `run-cancellation-fence` on supersede) | — |
| Verdict | `obtain-verdict` **<** `reflect-verdict`; `record-decision` **<** `reflect-verdict` on the decision path | `open-gates`, `escalate-gate`, `arm-park-escalation`, `park`, `unpark` |
| Plan | `evaluate-payment-auth-eligibility` **<** `construct-and-freeze-plan` **<** `begin-fulfillment` | `evaluate-activation-eligibility` |
| Waves | `begin-fulfillment` **<** `dispatch-wave1-create` **<** `re-check-pre-activation` **<** `report-spawn-signal` **<** `dispatch-wave2-activate` | `evaluate-activation-eligibility`, `reconcile-intent`, `reread-draft-liveness`, `rebuild-wave1` |
| Failure | `create-manual-task` before any terminal outcome under `remediate` | `resolve-manual-task`, `verify-override`, `retry-step`, `raise-overdue-escalation` |
| Unwind | `run-cancellation-fence` **<** `compensate-order` **<** `report-outcome` on every failure, cancel, supersede and terminal-event path; `authorize-cancel` **<** `run-cancellation-fence` on the cancel path; `terminate-on-terminal-event` **<** `run-cancellation-fence` on the terminal-event path | — |
| Hold | `apply-hold` **<** `apply-resume`, in the arm that owns the escalation `wait` | — |
| Termination | `terminate-instance` last on every path; `report-outcome` **<** `terminate-instance` where an outcome is reported | — |

`settle-from-lookup` **MUST NOT** appear. No path **MAY** reach `report-outcome` with
`outcome: completed` except through `dispatch-wave2-activate`, and none **MAY** reach it with
`failed`, `cancelled` or `superseded` except through `compensate-order`. **Rationale**: these are
the steps the PRD's acceptance criteria and the seam rules R1–R5 make non-negotiable; everything
else is the flow's to arrange.

### 4.2 The validation hook contract

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-constraint-definition-validation-hook`

The validation hook of §3.2 **MUST** run the eight rules of §2.2 *Validation before publish* over
every candidate version before the platform registry publishes it, **and** the same rules
**MUST** run as a CI test over the repository copy of every canonical definition on every change
to `definitions/` or to any slice's §3.3 declaration. A refusal **MUST** name the rule and the
task location in the platform's `ValidationError` shape. A version **MUST NOT** be published from
outside the pipeline that runs the test. The hook **MUST** also refuse the platform's `archive`
and `delete` transitions for a version an `owf_definition_binding` names, and the CI test **MUST**
assert that every version named by a binding in each environment is still published there.

### 4.3 Pinning

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-constraint-definition-pinning`

An instance **MUST** run to termination on the version `start-instance` bound
([`01 §3.7`](./01-foundation.md#table-owf_definition_binding)); no operation **MAY** rewrite the
binding, no operator surface **MAY** migrate an instance (PRD §5.2), and a new version **MUST**
affect only instances started after its publication. The platform's own pin of the invocation to
the callable version ([`DESIGN.md:614`](../../../../serverless-runtime/docs/DESIGN.md#versioning-model))
and Orders' binding **MUST** agree; `start-instance` records the version the invocation reports
(`$workflow.definition.version`) and a disagreement between that value and the version the
release pipeline marked active is an alert, not a silent choice.

### 4.4 Signal semantics

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-constraint-definition-signals`

Hold and resume **MUST** be consumed as the Lifecycle events `OrderHeld` and `OrderResumed`
through `listen`, never as platform `suspend`/`resume` control actions, because a platform
suspension would pause every `wait` including the lifetime ceiling. An operator cancel **MUST** be
delivered as the `cancel-requested` signal to the running invocation and **MUST NOT** use the
platform's generic `cancel`, which ends the invocation without the fence. A re-authorisation
**MUST** be delivered as `reauthorize-requested`. Every signal **MUST** be recorded in Orders
before delivery (the request row of the originating control operation, `09`) and **MUST** be
delivered idempotently under the caller-side duplicate protocol; a signal with no consuming arm in
the current stage **MUST** be held by the plugin's subscription until an arm consumes it, and if
the platform cannot guarantee that, the originating control operation **MUST** answer
`still-processing` until Orders observes the arm's recording operation (`authorize-cancel`,
`evaluate-payment-auth-eligibility`, `resolve-manual-task`). The exact `:plugin-control` verb and payload are an
upstream ask (§3.3); this rule binds regardless of their shape.

### 4.5 The hold pattern, and Q-11

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-constraint-definition-hold-pattern`

The escalation `wait` **MUST** be inside a competing `fork` that also contains the hold arm, so
that a hold cancels it; `apply-hold` and `apply-resume` **MUST** return the remaining window
slice 03's gate-window port computed from Orders' record, and the definition **MUST** re-arm
exactly the remainder `apply-resume` returned. The lifetime `wait` **MUST** be a top-level
competing arm outside every stage fork, and the barrier poll, the expected-fulfillment `wait` and
the overdue `wait` **MUST** be in branches a hold arm does not cancel: outside the approval stage
a hold is recorded and the stage loop continues (§3.6 (e)). **Q-11 is registered open**
(`../DECISIONS.md`): whether the Serverless Workflow DSL 1.0.0, as the platform's plugin
implements it, (i) accepts a runtime expression as a `wait` duration, so the remainder and the
expected-fulfillment instant can be armed without a Function; (ii) surfaces the Problem body's
`error_code` on `$error` so a `catch` can tell `idempotency-key-conflict` from `still-processing`
before the retry budget is spent; (iii) offers any dynamic parallel construct, so a wave could
fan out per line inside the definition rather than inside the operation; and (iv) treats a
`listen` inside a competing `fork` as cancellable without losing an event delivered during
cancellation; and (v) expresses **the hold pattern** as a whole — a hold arm that wins a
competing `fork` to cancel the escalation `wait`, a resume wait, and a re-armed `wait` of the
remainder Orders returns — natively, without a Function. Until Q-11 is answered, (i) falls back
to a registered Function that sleeps the remainder, (ii) is bounded by the retry budget, (iii) is
settled as one `call` per wave carrying `lineRefs[]`, (iv) is covered by the poll arms and the
re-entry of every stage loop, and (v) falls back to the same Function as (i) for the re-armed
`wait` (open question Q-11: Q-11 carries the five sub-questions (i)–(v)).

### 4.6 Protected operations are never inside a swallowing `catch`

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-constraint-definition-no-swallowing-catch`

A `try` whose task list contains a `protected` operation **MUST** have a `catch` that either
carries only `retry` (so exhaustion propagates the error) or ends in a `raise` or a `then` into
the stage's failure arm. A `catch` that returns normally after a protected operation's permanent
failure — continuing the path as if the step had settled — is refused before publish (§2.2 rule
6). **Rationale**: a swallowed failure of `reflect-verdict`, `begin-fulfillment`,
`report-spawn-signal` or `report-outcome` is precisely the divergence between process state and
order state the dual-authority principle exists to prevent; Orders' record would show the step
failed while the definition proceeds as if it had not.

### 4.7 What a definition change may and may not do

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-workflow-constraint-definition-change-scope`

A new version **MAY** reorder `composable` operations within a stage, change any `wait` inside
the nesting rule, change retry attempts and backoff, change `switch` predicates over returned
enums, and add or remove a `listen` arm whose type is in the closed set. A new version **MUST
NOT** introduce an operation name not in `owf_step_operation`, a `listen` type outside the closed
set, a `run` or `emit` task, a `body` member outside the reference schemas, or an `emit` of any
of the six process events. Anything in the second list is an Orders release and a change to the
slice that owns it.

## 5. Traceability

- **PRD**: [`../PRD.md`](../PRD.md) — §6.1–§6.4 (the paths), §5.2 (no migration), §15 Q-01 (history isolation criteria), §17.1 (the process-flow diagram these fragments reproduce)
- **ADRs**: [`ADR/0011`](../ADR/0011-cpt-cf-bss-orders-workflow-adr-flow-as-platform-definition.md) flow as platform definition; [`ADR/0012`](../ADR/0012-cpt-cf-bss-orders-workflow-adr-definition-versioning-and-protected-steps.md) definition versioning and protected steps; [`ADR/0013`](../ADR/0013-cpt-cf-bss-orders-workflow-adr-references-not-payloads.md) references not payloads; [`ADR/0004`](../ADR/0004-cpt-cf-bss-orders-workflow-adr-two-wave-activation-barrier.md) two-wave barrier (as amended: the conjunction as a definition pattern); [`ADR/0005`](../ADR/0005-cpt-cf-bss-orders-workflow-adr-saga-compensable-no-pivot.md) compensable no pivot (as amended: `try`/`catch` around an Orders-owned walk); [`ADR/0007`](../ADR/0007-cpt-cf-bss-orders-workflow-adr-fail-closed-verdict-park.md) fail-closed park (as amended: park as a definition arm)
- **Platform**: [serverless-runtime DESIGN](../../../../serverless-runtime/docs/DESIGN.md) §1.1, §1.4, §3.1 (*Functions and Workflows*, *Workflow*, *Versioning Model*, *Invocation Status State Machine*), §3.2 (*Function Registry*), §3.3 (*Function Registry API*, *Invocation API*, *Event Trigger Management API*); [ADR-0003](../../../../serverless-runtime/docs/ADR/0003-cpt-cf-serverless-runtime-adr-workflow-dsl.md); [ADR-0004](../../../../serverless-runtime/docs/ADR/0004-cpt-cf-serverless-runtime-adr-temporal-workflow-engine.md); [ADR-0005](../../../../serverless-runtime/docs/ADR/0005-cpt-cf-serverless-runtime-adr-thin-host.md)
- **Design set**: [`./README.md`](./README.md); [`./01-foundation.md`](./01-foundation.md) (the envelope, the operations, the binding); [`02`](./02-triggers-and-start.md)–[`09`](./09-read-and-authz.md) (the operations each path calls)
- **Related requirements**: `cpt-cf-bss-orders-workflow-fr-owf-start-contract`, `cpt-cf-bss-orders-workflow-fr-owf-approval-request`, `cpt-cf-bss-orders-workflow-fr-owf-approval-escalation`, `cpt-cf-bss-orders-workflow-fr-owf-approval-decision`, `cpt-cf-bss-orders-workflow-fr-owf-payment-auth`, `cpt-cf-bss-orders-workflow-fr-owf-fulfillment-plan`, `cpt-cf-bss-orders-workflow-fr-owf-provisioning-intent`, `cpt-cf-bss-orders-workflow-fr-owf-line-progress`, `cpt-cf-bss-orders-workflow-fr-owf-retry`, `cpt-cf-bss-orders-workflow-fr-owf-manual-task`, `cpt-cf-bss-orders-workflow-fr-owf-compensation-execution`, `cpt-cf-bss-orders-workflow-fr-owf-hold-resume`, `cpt-cf-bss-orders-workflow-fr-owf-overdue-escalation`, `cpt-cf-bss-orders-workflow-fr-owf-terminal-order-events`, `cpt-cf-bss-orders-workflow-fr-owf-process-state-nonauth`
