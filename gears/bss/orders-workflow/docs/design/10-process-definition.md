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
| `cpt-cf-bss-orders-workflow-fr-owf-start-contract` | The definition starts on `OrderSubmitted` through a platform event trigger (§3.3) and `listen`s for the other eight Lifecycle triggers inside the running invocation, correlated on `orderId`/`orderVersion` (§3.6 (a), (e), (f)); admission and supersession remain Orders' `admit-trigger`. |
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
(`OrderSubmitted` as the start trigger; `OrderApproved`, `OrderAmended`, `OrderHeld`,
`OrderResumed`, `OrderAcceptanceRecorded`, `OrderCancelled`, `OrderExpired`, `OrderRejected` as
`listen` targets), whose GTS identifiers are Lifecycle's
(`gts.cf.core.events.event.v1~cf.bss.orders.event.v1~cf.bss.orders.<name>.v1~`,
[Lifecycle `01 §4.4`](../../../orders-lifecycle/docs/design/01-foundation.md#44-events-audit-and-the-outbox-normative));
the approval decision event of [`03 §3.3`](./03-approval-execution.md#33-api-contracts); the
two Subscriptions outcome events of [`05 §3.5`](./05-provisioning-intents.md#35-external-dependencies);
Orders' own `OrderFulfillmentCompleted` and `OrderFulfillmentAborted` (used only to cancel the
overdue arm); and the operator signal types of §3.3. Every filter **MUST** `correlate` on
`orderId` and `orderVersion` against `$context`. Any other type is refused before publish.

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
7. [ ] - `p1` - No `run`, no `emit`, no `for`; `schedule.on` names only `OrderSubmitted` - `inst-def-grammar-subset`
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
a seller-scoped fragment role is registered as **Q-10** (`../DECISIONS.md`, commit D). **Audit of
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
arm: `cancel-requested` (an authorised workflow-mediated cancel, [`09 §3.3`](./09-read-and-authz.md#33-api-contracts))
and `reauthorize-requested` (a payment re-authorisation, [`04`](./04-fulfillment-plan.md)). Hold
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
    TRG[Event trigger: OrderSubmitted → order_process]
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
stated in the serverless-runtime design and is an **upstream ask** (commit D); until it is
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
| Start an invocation | `POST /api/serverless-runtime/v1/invocations` with `function_id`, `mode: async`, `params`, `Idempotency-Key` | `DESIGN.md:865`, `DESIGN.md:895`–`910` | Not called by Orders in normal operation — the event trigger starts the invocation; used by an operator re-drive of a dead invocation (`09`) with `Idempotency-Key = {tenant}:{orderId}:{orderVersion}:order-process` so a duplicate start is deduplicated by the platform |
| Read invocation status | `GET /api/serverless-runtime/v1/invocations/{invocation_id}` | `DESIGN.md:867` | The backstop sweep (`01 §3.8`) and the progress read (`09`) |
| Generic control | `POST …/invocations/{invocation_id}:control` (`cancel`, `suspend`, `resume`, `retry`, `replay`) | `DESIGN.md:868`, `DESIGN.md:883`–`889` | `retry` from `failed` by an operator re-drive only; `cancel` **never** for an order cancel (§3.2); `suspend`/`resume` **never** — hold is a definition arm, not a platform suspension |
| Plugin control (signals) | `POST …/invocations/{invocation_id}:plugin-control` | `DESIGN.md:869`, `DESIGN.md:893` | Delivery of `cancel-requested` and `reauthorize-requested` to the running invocation's `listen` arms |
| Event trigger binding | `/api/serverless-runtime/v1/event-triggers` (create, enable, disable, metrics) | `DESIGN.md:980`–`987` | One trigger binding `OrderSubmitted` to `order_process`, filtered to `category = new_sale`; its dead-letter handling is the platform's (`01 §4.8`) |
| Timeline (debug) | `GET …/invocations/{invocation_id}/timeline` | `DESIGN.md:1061` | Operator debugging only; never an Orders read path |

**Signals.** The four operator-facing instructions map as follows:

| Instruction | Origin | Delivery | Definition arm | Operation that records it |
|-------------|--------|----------|----------------|---------------------------|
| hold | Lifecycle `OrderHeld` | Broker event, correlated `listen` | §3.6 (e) | `apply-hold` (08) |
| resume | Lifecycle `OrderResumed` | Broker event, correlated `listen` | §3.6 (e) | `apply-resume` (08) |
| cancel | Seller operator, `POST /bss-orders-workflow/v1/workflows/{orderId}/cancel` (`09 §3.3`) | `…:plugin-control` signal `cancel-requested` | §3.6 (d) | `authorize-cancel` (08) |
| re-authorise | Operator / Payments outcome (`04`) | `…:plugin-control` signal `reauthorize-requested` | §3.6 (b) | `evaluate-payment-auth-eligibility` (04) |

The platform states that the plugin "owns the verb set its backend supports" on
`:plugin-control` (`DESIGN.md:893`) and that Temporal signals are native (ADR-0004 *Option A*);
it does **not** state a verb or payload shape for delivering a named signal that a `listen` task
consumes. **The exact plugin-control payload is an upstream ask** (`UPSTREAM_REQS.md`, commit D).
Until answered, the signal types are declared here as reference names —
`gts.cf.core.events.event.v1~cf.bss.orders_workflow.signal.v1~cf.bss.orders_workflow.cancel_requested.v1~`
and `…reauthorize_requested.v1~` — and are **never published to the broker**: they are not among
the six process events and carry no `data` beyond the reference tuple and a `requestRef`.

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
`step: <operation>` expands to the `call: http` form of the first fragment; the word
**protected** marks operations the fence of §4 requires. Every `body` member is a reference or an
enum (§2.1). Values marked *Q-11* depend on the DSL accepting a runtime expression where shown.

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
schedule:
  on:
    one:
      with:
        type: gts.cf.core.events.event.v1~cf.bss.orders.event.v1~cf.bss.orders.submitted.v1~
input:
  from: >-
    ${ { orderId: .data.orderId, orderVersion: .data.orderVersion,
         resourceTenantId: .data.resourceTenantId, triggerEventId: .id } }
do:
  - admitTrigger:                       # protected (02)
      call: http
      with:
        method: post
        endpoint: ${ $context.stepsBase + "/admit-trigger" }
        headers:
          Idempotency-Key: ${ .resourceTenantId + ":" + .triggerEventId + ":admit-trigger" }
        body:
          orderId: ${ .orderId }
          orderVersion: ${ .orderVersion }
          resourceTenantId: ${ .resourceTenantId }
          triggerEventId: ${ .triggerEventId }
          invocationId: ${ $workflow.id }
          attemptId: ${ $workflow.id + ":" + $task.name }
      export:
        as: ${ $context + { correlationId: .correlationId, admission: .admission } }
  - onAdmission:
      switch:
        - absorbed:
            when: ${ $context.admission == "absorbed-duplicate" }
            then: end
        - proceed:
            then: startInstance
  - startInstance:                      # protected (01)
      step: start-instance              # body: ref + definitionId, definitionVersion: $workflow.definition.version, definitionSource: platform
      export: { as: '${ $context + { rowVersion: .rowVersion } }' }
  - lifetime:
      fork:
        compete: true
        branches:
          - process:
              do:
                - approval: { do: [ obtainVerdict, onVerdict, reflectVerdict, afterReflect, openGates, gateLoop, afterGateLoop ] }   # this fragment
                - fulfillment: { do: [ ...fragment (b) ] }
                - arm: { set: { arm: process } }
          - lifetimeCeiling:
              do:
                - waitCeiling: { wait: { days: 90 } }
                - escalate: { step: raise-overdue-escalation }     # body: ref + escalationKind: lifetime-ceiling
                - parkCeiling: { step: park }                      # body: ref + parkReason: lifetime-ceiling, subjectRef: process
                - arm: { set: { arm: lifetime } }
  - afterLifetime:
      switch:
        - parked:   { when: '${ .arm == "lifetime" }', then: awaitOperatorAfterPark }   # (d) or unpark via signal
        - done:     { then: end }
```

The approval stage, inside `process.approval`:

```yaml
- obtainVerdict:                        # protected (03)
    try:
      - call: { step: obtain-verdict }  # output: verdict ∈ required | not-required | unobtainable; authorityRef
    catch:
      errors: { with: { status: 503 } }
      retry: transient
    export: { as: '${ $context + { verdict: .verdict } }' }
- onVerdict:
    switch:
      - unobtainable: { when: '${ $context.verdict == "unobtainable" }', then: parkForVerdict }
      - obtained:     { then: reflectVerdict }
- parkForVerdict:                       # fail-closed park, ADR-0007 as amended
    do:
      - park: { step: park }            # body: ref + parkReason (03 §3.7 value), subjectRef: verdict
      - armEscalation: { step: arm-park-escalation }   # output: escalateAfter (duration to the Lifecycle submitted-TTL margin)
      - parkLoop:
          fork:
            compete: true
            branches:
              - retryVerdict:
                  do:
                    - waitRetry: { wait: { minutes: 5 } }
                    - again: { step: obtain-verdict }
                    - arm: { set: { arm: retried, verdict: '${ .verdict }' } }
              - escalation:
                  do:
                    - waitTtlMargin: { wait: '${ $context.escalateAfter }' }        # Q-11
                    - escalate: { step: raise-overdue-escalation }                 # body: ref + escalationKind: park
                    - arm: { set: { arm: escalated } }
              - cancel: { do: [ { awaitCancel: { listen: { to: { one: { with: { type: <cancel-requested signal> }, correlate: { orderId: { from: '${ .data.orderId }', expect: '${ $context.orderId }' }, orderVersion: { from: '${ .data.orderVersion }', expect: '${ $context.orderVersion }' } } } } } } }, { arm: { set: { arm: cancel } } } ] }
      - afterParkLoop:
          switch:
            - obtained: { when: '${ .arm == "retried" and .verdict != "unobtainable" }', then: unparkForVerdict }
            - still:    { when: '${ .arm == "retried" or .arm == "escalated" }', then: parkLoop }
            - cancel:   { then: cancelPath }                                          # fragment (d)
      - unparkForVerdict: { step: unpark, then: reflectVerdict }
- reflectVerdict:                       # protected (03): reflects submitted → pending_approval | approved under R1
    step: reflect-verdict
    export: { as: '${ $context + { reflected: .reflected } }' }   # reflected ∈ pending_approval | approved | rejected
- afterReflect:
    switch:
      - approved: { when: '${ $context.reflected == "approved" }', then: fulfillment }   # fragment (b)
      - rejected: { when: '${ $context.reflected == "rejected" }', then: terminateRejected }
      - pending:  { then: openGates }
- openGates:                            # composable (03): opens every gate at the current sequence position
    step: open-gates                    # output: gateRefs[], escalationWindow (duration), position
    export: { as: '${ $context + { gateRefs: .gateRefs, escalationRemaining: .escalationWindow } }' }
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
              - recordDecision: { step: record-decision }    # protected (03); body: ref + gateRef, decisionEventId, outcome ∈ approved | rejected
              - arm: { set: { arm: decision, gateState: '${ .gateState }' } }   # gateState ∈ approved | rejected | next-position | pending
        - escalation:
            do:
              - waitEscalation: { wait: '${ $context.escalationRemaining }' }   # Q-11
              - escalateGate: { step: escalate-gate }        # composable (03); enqueues OrderApprovalEscalated inside the operation
              - arm: { set: { arm: escalated } }
        - hold: { do: [ ...fragment (e), { arm: { set: { arm: resumed } } } ] }
        - amendment: { do: [ ...fragment (f) amendment listen, { arm: { set: { arm: superseded } } } ] }
        - cancel: { do: [ ...cancel-requested listen as above, { arm: { set: { arm: cancel } } } ] }
- afterGateLoop:
    switch:
      - allApproved:   { when: '${ .arm == "decision" and .gateState == "approved" }',      then: reflectVerdict }   # pending_approval → approved; Lifecycle then emits OrderApproved
      - rejected:      { when: '${ .arm == "decision" and .gateState == "rejected" }',      then: reflectVerdict }   # pending_approval → rejected → terminateRejected
      - nextPosition:  { when: '${ .arm == "decision" and .gateState == "next-position" }', then: openGates }
      - escalated:     { when: '${ .arm == "escalated" }', then: rearmFull }
      - resumed:       { when: '${ .arm == "resumed" }',   then: gateLoop }          # escalationRemaining re-exported by apply-resume
      - superseded:    { when: '${ .arm == "superseded" }', then: supersede }        # fragment (f)
      - cancel:        { when: '${ .arm == "cancel" }',    then: cancelPath }        # fragment (d)
      - pending:       { then: gateLoop }
- rearmFull: { set: { escalationRemaining: '${ $context.escalationWindow }' }, then: gateLoop }
- terminateRejected: { step: terminate-instance, then: end }   # protected (01); terminationKind: rejected
```

**Description**: The fragment reproduces [`02 §3.6`](./02-triggers-and-start.md#36-interactions--sequences)
*Start on trigger* and *Duplicate absorption* and [`03 §3.6`](./03-approval-execution.md#36-interactions--sequences)
*Verdict retrieval and reflection*, *Multi-party gate open* and *Decision reflection*, with the
timer and the pause moved into the definition: the escalation window is the `wait` in the
`escalation` branch, and because the `fork` competes, a decision, a hold or a cancel cancels that
`wait` — which is exactly the pause `03 §3.6` *Escalation timer fire and approval-service outage
pause* used to write into `owf_timer_pause`. The approval-service outage pause of that sequence
is expressed the same way: `escalate-gate` answers `retryable-failure` while the breaker is open,
the `try` around it retries under `transient`, and on exhaustion `raise-overdue-escalation` is
called with `escalationKind: approval-outage` (not shown). A repeat `obtain-verdict` for a version
whose verdict is already reflected returns the cached answer inside the operation and never
re-reflects, as `03` requires; the definition does not need to know. The approved path ends at
`reflect-verdict` (`pending_approval → approved`); Lifecycle emits `OrderApproved`, and the
fulfillment stage is entered from `afterReflect` in the same invocation rather than from a second
trigger, because the instance already exists and `admit-trigger` would resolve the redelivered
event to `absorbed-duplicate`.

#### (b) Fulfillment: eligibility, plan, two waves and the barrier

**ID**: `cpt-cf-bss-orders-workflow-seq-def-fulfillment`

**Use cases**: `cpt-cf-bss-orders-workflow-fr-owf-payment-auth`, `cpt-cf-bss-orders-workflow-fr-owf-fulfillment-plan`, `cpt-cf-bss-orders-workflow-fr-owf-provisioning-intent`, `cpt-cf-bss-orders-workflow-fr-owf-line-progress`, `cpt-cf-bss-orders-workflow-fr-owf-overdue-escalation`

**Actors**: `cpt-cf-bss-orders-workflow-actor-owf-orders-lifecycle`, `cpt-cf-bss-orders-workflow-actor-owf-payments`, `cpt-cf-bss-orders-workflow-actor-owf-subscriptions`

```yaml
- fulfillment:
    do:
      - eligibility:                                # protected (04)
          step: evaluate-payment-auth-eligibility   # output: eligibility ∈ eligible | pending | withheld
          export: { as: '${ $context + { eligibility: .eligibility } }' }
      - onEligibility:
          switch:
            - eligible: { when: '${ $context.eligibility == "eligible" }', then: freezePlan }
            - withheld: { when: '${ $context.eligibility == "withheld" }', then: awaitEligibilityChange }   # order stays approved
            - pending:  { then: awaitEligibilityChange }
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
                    - arm: { set: { arm: reevaluate } }
              - reauthorize:
                  do:
                    - listenReauth: { listen: { to: { one: { with: { type: <reauthorize-requested signal> }, correlate: { orderId: { from: '${ .data.orderId }', expect: '${ $context.orderId }' }, orderVersion: { from: '${ .data.orderVersion }', expect: '${ $context.orderVersion }' } } } } } }
                    - arm: { set: { arm: reevaluate } }
              - hold:      { do: [ ...fragment (e), { arm: { set: { arm: reevaluate } } } ] }
              - amendment: { do: [ ...fragment (f) listen, { arm: { set: { arm: superseded } } } ] }
              - cancel:    { do: [ ...cancel-requested listen, { arm: { set: { arm: cancel } } } ] }
      - afterEligibilityChange:
          switch:
            - again:      { when: '${ .arm == "reevaluate" }', then: eligibility }
            - superseded: { when: '${ .arm == "superseded" }', then: supersede }
            - cancel:     { then: cancelPath }
      - freezePlan:                                 # protected (04)
          try:
            - call: { step: construct-and-freeze-plan }   # output: planRef, lineRefs[], expectedFulfillmentAt, policy ∈ remediate | fail-fast, planState ∈ frozen | invalid-graph | topology-unavailable
          catch: { errors: { with: { status: 503 } }, retry: transient }
          export: { as: '${ $context + { planRef: .planRef, lineRefs: .lineRefs, expectedFulfillmentAt: .expectedFulfillmentAt, policy: .policy, planState: .planState } }' }
      - onPlan:
          switch:
            - frozen:  { when: '${ $context.planState == "frozen" }', then: beginFulfillment }
            - invalid: { then: partialFailure }        # fragment (c): plan-level manual task or incident per pinned policy; nothing to compensate
      - beginFulfillment:                           # protected (04): the R1 seam call approved → in_fulfillment; enqueues OrderFulfillmentStarted
          step: begin-fulfillment
      - fulfillmentWithOverdue:
          fork:
            compete: true
            branches:
              - waves:
                  do:
                    - wave1:                              # protected (05); one call, the line set as references; per-order parallelism inside (05 admission)
                        try:
                          - call: { step: dispatch-wave1-create }   # body: ref + planRef, lineRefs; output: accepted[], failed[] (lineRef + reason)
                          catch: { errors: { with: { status: 503 } }, retry: transient, do: [ { toFailure: { raise: { error: { type: 'https://serverlessworkflow.io/spec/1.0.0/errors/communication', status: 503, title: wave1-exhausted } } } } ] }
                        export: { as: '${ $context + { wave1Failed: .failed } }' }
                    - onWave1:
                        switch:
                          - anyFailed: { when: '${ ($context.wave1Failed | length) > 0 }', then: partialFailure }   # fragment (c)
                          - allAccepted: { then: barrier }
                    - barrier:                            # conjunction: expected time reached AND every create confirmed
                        do:
                          - waitExpected: { wait: '${ $context.expectedFulfillmentAt }' }   # Q-11: duration to the instant; zero when already past
                          - barrierLoop:
                              do:
                                - evaluate:               # composable (04): reads Orders' record of confirmations; never the definition's memory
                                    step: evaluate-activation-eligibility   # output: released (bool), eligibleLineRefs[], pendingLineRefs[]
                                    export: { as: '${ $context + { released: .released, eligibleLineRefs: .eligibleLineRefs } }' }
                                - onEvaluate:
                                    switch:
                                      - released: { when: '${ $context.released }', then: preActivation }
                                      - pending:  { then: awaitConfirmationOrPoll }
                                - awaitConfirmationOrPoll:     # re-evaluate on EVERY contributing signal: a confirmation event OR the poll interval
                                    fork:
                                      compete: true
                                      branches:
                                        - confirmation:
                                            do:
                                              - listenConfirmation:
                                                  listen:
                                                    to:
                                                      any:
                                                        - with: { type: <Subscriptions draft-create outcome event, 05 §3.5> }
                                                          correlate: { orderId: { from: '${ .data.orderId }', expect: '${ $context.orderId }' }, orderVersion: { from: '${ .data.orderVersion }', expect: '${ $context.orderVersion }' } }
                                              - arm: { set: { arm: signal } }
                                        - poll:
                                            do:
                                              - waitPoll: { wait: { seconds: 30 } }
                                              - reconcile: { step: reconcile-intent }   # composable (05): status read; settles a dead lease through settle-from-lookup in-process
                                              - arm: { set: { arm: poll } }
                                        - hold:      { do: [ ...fragment (e), { arm: { set: { arm: poll } } } ] }
                                        - amendment: { do: [ ...fragment (f) listen, { arm: { set: { arm: superseded } } } ] }
                                        - cancel:    { do: [ ...cancel-requested listen, { arm: { set: { arm: cancel } } } ] }
                                - afterSignal:
                                    switch:
                                      - again:      { when: '${ .arm == "signal" or .arm == "poll" }', then: evaluate }
                                      - superseded: { when: '${ .arm == "superseded" }', then: supersede }
                                      - cancel:     { then: cancelPath }
                    - preActivation:                      # protected (04): SUB-O5 overlap presence + market + authorization freshness, immediately before wave 2
                        step: re-check-pre-activation     # output: verdict ∈ proceed | abort; abortReason
                        export: { as: '${ $context + { preActivation: .verdict } }' }
                    - onPreActivation:
                        switch:
                          - abort:   { when: '${ $context.preActivation == "abort" }', then: compensateOrder }   # fragment (c): void wave-1 drafts, fulfillment_failed with the abort reason
                          - proceed: { then: spawnSignal }
                    - spawnSignal:                        # protected (05): the first activation intent is the Lifecycle spawn/fencing signal
                        step: report-spawn-signal
                    - wave2:                              # protected (05); one call, eligible lines as references; draft-liveness re-read and wave-1 rebuild inside (05)
                        try:
                          - call: { step: dispatch-wave2-activate }   # body: ref + planRef, lineRefs: $context.eligibleLineRefs; output: activated[], failed[], pending[]
                          catch: { errors: { with: { status: 503 } }, retry: transient, do: [ { toFailure: { raise: { error: { type: 'https://serverlessworkflow.io/spec/1.0.0/errors/communication', status: 503, title: wave2-exhausted } } } } ] }
                        export: { as: '${ $context + { wave2Failed: .failed, wave2Pending: .pending } }' }
                    - onWave2:
                        switch:
                          - anyFailed:  { when: '${ ($context.wave2Failed | length) > 0 }', then: partialFailure }   # fragment (c), wave-2 partial failure (D-54)
                          - anyPending: { when: '${ ($context.wave2Pending | length) > 0 }', then: barrier }        # dependents wait for their dependencies; the same conjunction loop
                          - complete:   { then: reportCompleted }
                    - reportCompleted:                    # protected (06): in_fulfillment → completed with per-line subscription ids; enqueues OrderFulfillmentCompleted
                        step: report-outcome              # body: ref + outcome: completed
                    - terminateCompleted: { step: terminate-instance }   # protected (01); terminationKind: completed
                    - arm: { set: { arm: done } }
              - overdue:                                  # the process deadline: 24 h past expected fulfillment time, raises an escalation and nothing else
                  do:
                    - waitOverdue: { wait: '${ $context.expectedFulfillmentAt + 24h }' }   # Q-11: duration to the instant
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
                    - keepRunning: { wait: { days: 90 } }   # the overdue arm never wins the outer race; the waves branch or a cancel/supersede completes it
                    - arm: { set: { arm: overdue } }
      - afterFulfillment:
          switch:
            - done: { then: end }
```

**Description**: The fragment reproduces [`04 §3.6`](./04-fulfillment-plan.md#36-interactions--sequences)
*Plan Construction and Freeze*, *Pre-Activation Abort* and *Per-Line Progress*, and
[`05 §3.6`](./05-provisioning-intents.md#36-interactions--sequences) *Two-Wave Provisioning
Dispatch*, *Wave-1 Rebuild* and *Reconciliation Sweep Cycle*. **The barrier is a conjunction and
is re-evaluated on every contributing signal** — that is stated in the structure, not left to a
reader: `waitExpected` supplies the timer half once; `evaluate-activation-eligibility` reads the
all-creates half from Orders' record, which Orders' own consumer of the Subscriptions outcome
events maintains ([`09 §3.6` *System-actor delivery over the event broker*](./09-read-and-authz.md#36-interactions--sequences)),
so the definition never holds the confirmation count itself; and `awaitConfirmationOrPoll`
returns to `evaluate` on **either** a confirmation event **or** the poll interval, so a
confirmation that landed between an evaluation and the `listen` cannot hang the barrier — the
timer fire is never the sole trigger ([`../ADR/0004`](../ADR/0004-cpt-cf-bss-orders-workflow-adr-two-wave-activation-barrier.md)
as amended). The poll arm is also the sweep cadence of `05 §4.2` for a live invocation: each pass
calls `reconcile-intent`, which settles a dead lease through `settle-from-lookup`
([`01 §3.6`](./01-foundation.md#36-interactions--sequences)). Per-line parallelism is inside the
two dispatch operations (§2.2), so a wave is one `call`; a wave's per-line outcomes come back as
reference lists and route the `switch`. The overdue arm runs beside the waves in a
non-terminating position: it can escalate, but it never cancels fulfillment, which is why it is
inside its own `fork` and completes only against the order's terminal event
([`07`](./07-manual-tasks.md), `cpt-cf-bss-orders-workflow-fr-owf-overdue-escalation`).

#### (c) Partial failure: manual task, resume or compensate

**ID**: `cpt-cf-bss-orders-workflow-seq-def-partial-failure`

**Use cases**: `cpt-cf-bss-orders-workflow-fr-owf-retry`, `cpt-cf-bss-orders-workflow-fr-owf-manual-task`, `cpt-cf-bss-orders-workflow-fr-owf-override-semantics`, `cpt-cf-bss-orders-workflow-fr-owf-compensation-execution`

**Actors**: `cpt-cf-bss-orders-workflow-actor-owf-fulfillment-operator`, `cpt-cf-bss-orders-workflow-actor-owf-subscriptions`, `cpt-cf-bss-orders-workflow-actor-owf-orders-lifecycle`

```yaml
- partialFailure:                                   # entered from a wave's failed[] list, from an exhausted retry (the raise in the wave's catch), or from an invalid plan
    do:
      - onPolicy:
          switch:
            - remediate: { when: '${ $context.policy == "remediate" }', then: createTasks }
            - failFast:  { then: compensateOrder }   # 07: fail-fast records a tracked incident inside compensate-order's path; no actionable task
      - createTasks:                                # protected (07): exactly one actionable task per failed line, reopen semantics inside
          step: create-manual-task                  # body: ref + planRef, failedLineRefs (from wave1Failed / wave2Failed), reason enums; output: taskRefs[]
          export: { as: '${ $context + { taskRefs: .taskRefs } }' }
      - awaitResolution:
          fork:
            compete: true
            branches:
              - resolution:
                  do:
                    - listenResolution: { listen: { to: { one: { with: { type: <manual-task resolution signal, 07 §3.3> }, correlate: { orderId: { from: '${ .data.orderId }', expect: '${ $context.orderId }' }, orderVersion: { from: '${ .data.orderVersion }', expect: '${ $context.orderVersion }' } } } } } }
                    - resolve: { step: resolve-manual-task }   # composable (07); output: resolution ∈ retry | override | escalate | exhausted; taskRef; attemptKey (from retry-step, 01)
                    - arm: { set: { arm: resolved, resolution: '${ .resolution }', attemptKey: '${ .attemptKey }' } }
              - hold:      { do: [ ...fragment (e), { arm: { set: { arm: held } } } ] }
              - amendment: { do: [ ...fragment (f) listen, { arm: { set: { arm: superseded } } } ] }
              - cancel:    { do: [ ...cancel-requested listen, { arm: { set: { arm: cancel } } } ] }
      - afterResolution:
          switch:
            - retry:     { when: '${ .arm == "resolved" and .resolution == "retry" }',     then: barrier }          # re-enters the conjunction; the dispatch re-runs the failed line under attemptKey
            - override:  { when: '${ .arm == "resolved" and .resolution == "override" }',  then: verifyOverride }
            - exhausted: { when: '${ .arm == "resolved" and .resolution == "exhausted" }', then: compensateOrder }  # D-3: 3 failed attempts or SLA elapsed
            - escalate:  { when: '${ .arm == "resolved" and .resolution == "escalate" }',  then: awaitResolution }  # routed to a seller operator; the task stays open
            - held:      { when: '${ .arm == "held" }', then: awaitResolution }
            - superseded:{ when: '${ .arm == "superseded" }', then: supersede }
            - cancel:    { then: cancelPath }
      - verifyOverride:                             # composable (07): identity check through Subscriptions; registers the subscription as a compensation subject
          step: verify-override                     # output: verified (bool)
          export: { as: '${ $context + { overrideVerified: .verified } }' }
      - afterOverride:
          switch:
            - verified: { when: '${ $context.overrideVerified }', then: barrier }      # the line is activated; the conjunction decides whether the order completes
            - rejected: { then: awaitResolution }                                    # task remains open
- compensateOrder:                                  # protected (06): fence claim, the whole reverse walk, evidence — ONE operation because the ordinal is Orders'
    do:
      - fence: { step: run-cancellation-fence }     # protected (06); body: ref + trigger ∈ failure | cancel | terminal-event | supersede; the 5 steps and owf_cancellation_fence
      - compensate:
          try:
            - call: { step: compensate-order }      # output: compensationState ∈ complete | pending-escalation
            catch: { errors: { with: { status: 503 } }, retry: transient }
          export: { as: '${ $context + { compensationState: .compensationState } }' }
      - onCompensation:
          switch:
            - complete: { when: '${ $context.compensationState == "complete" }', then: reportFailed }
            - pending:  { then: awaitCompensationResolution }   # a leg failed: manual task already created by compensate-order (07 unconditional); order stays non-terminal
      - awaitCompensationResolution:
          fork:
            compete: true
            branches:
              - resolution: { do: [ ...manual-task resolution listen as above, { resolve: { step: resolve-manual-task } }, { arm: { set: { arm: resolved } } } ] }
              - retryLeg:   { do: [ { waitRetry: { wait: { hours: 1 } } }, { arm: { set: { arm: retry } } } ] }
      - afterCompensationResolution:
          switch:
            - again: { then: compensate }             # compensate-order is idempotent per subject; already-settled legs are absorbed
      - reportFailed:                               # protected (06): in_fulfillment → fulfillment_failed with compensation evidence, or the workflow-mediated cancel; enqueues OrderFulfillmentAborted
          step: report-outcome                      # body: ref + outcome ∈ failed | cancelled | superseded, trigger
      - terminateAborted: { step: terminate-instance, then: end }   # protected (01); terminationKind: compensated
```

**Description**: The fragment reproduces [`06 §3.6`](./06-saga-and-compensation.md#36-interactions--sequences)
*Order-Level Failure Compensation* and *Compensation Walk Order*, and
[`07 §3.6`](./07-manual-tasks.md#36-interactions--sequences) *Manual Task Created on Failure* and
*Override Rejected Without Verified Subscription*. Retry exhaustion is the definition's — the
`catch.retry` on a wave call — and what follows it is `create-manual-task`, exactly as
[`08 §3.6` *Transient Outage: Retry-Then-Manual-Task*](./08-hold-and-cancel.md#36-interactions--sequences)
draws it; the envelope creates no task (`01 §4.5`). **The reverse walk is one operation**:
`compensate-order` owns `compensation_sequence`, the descending walk, the per-subject phase
resolution and the `failed-pending-escalation` records
([`../ADR/0005`](../ADR/0005-cpt-cf-bss-orders-workflow-adr-saga-compensable-no-pivot.md) as
amended), because splitting the walk into definition tasks would put the ordinal — a fact
Orders' saga log is authoritative for — into engine state. The `try`/`catch` of the spec is
therefore the structure *around* compensation (when it runs, what happens when it cannot finish),
not the compensation itself. `report-outcome` is called only when the fence row's
`no_active_verified_at` is set, which `compensate-order` reports as `complete`; a
`pending-escalation` state loops through the manual task and never reports, which is the PRD's
"order remains non-terminal until operational compensation reaches a known outcome".

#### (d) Cancel

**ID**: `cpt-cf-bss-orders-workflow-seq-def-cancel`

**Use cases**: `cpt-cf-bss-orders-workflow-fr-owf-compensation-execution`, `cpt-cf-bss-orders-workflow-fr-owf-terminal-order-events`

**Actors**: `cpt-cf-bss-orders-workflow-actor-owf-seller-operator`, `cpt-cf-bss-orders-workflow-actor-owf-orders-lifecycle`, `cpt-cf-bss-orders-workflow-actor-owf-subscriptions`

```yaml
# the cancel arm of every competing fork above:
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
          export: { as: '${ $context + { cancelRequestRef: .data.requestRef } }' }
      - arm: { set: { arm: cancel } }
# the path the sibling switch routes to:
- cancelPath:
    do:
      - authorize:                                  # protected (08): the apply-time re-check of authority (09 §4.4); records the operator from Orders' own request record
          step: authorize-cancel                    # body: ref + cancelRequestRef; output: authorized (bool)
          export: { as: '${ $context + { cancelAuthorized: .authorized } }' }
      - onAuthorize:
          switch:
            - denied:     { when: '${ $context.cancelAuthorized | not }', then: resumeWhereLeft }   # the request is refused in Orders; the process continues its stage
            - authorized: { then: fenceForCancel }
      - fenceForCancel: { step: run-cancellation-fence }   # protected (06); trigger: cancel
      - compensateForCancel: { then: compensateOrder }     # fragment (c) from `compensate` on; report-outcome carries outcome: cancelled → workflow-mediated cancel with evidence
- awaitOperatorAfterPark:                           # after the lifetime ceiling parked the instance
    fork:
      compete: true
      branches:
        - cancel: { do: [ ...cancel arm ] }
        - unpark: { do: [ { listenUnpark: { listen: { to: { one: { with: { type: <unpark-requested signal> }, correlate: { orderId: { from: '${ .data.orderId }', expect: '${ $context.orderId }' } } } } } } }, { unpark: { step: unpark } }, { arm: { set: { arm: unparked } } } ] }
```

**Description**: The fragment reproduces [`06 §3.6`](./06-saga-and-compensation.md#36-interactions--sequences)
*Authorized-Cancellation Compensation* and *Five-Step Cancellation Fencing* and
[`08 §3.6` *Workflow-Mediated Cancel with Compensation Evidence*](./08-hold-and-cancel.md#36-interactions--sequences).
The cancel is a signal (§3.3) because the platform's generic `cancel` would end the invocation
without the fence; the arm exists in every competing `fork` of the process so a cancel is
consumable in every stage, and the `switch` after each fork routes it to one `cancelPath`.
`authorize-cancel` is where the request's authority is re-checked at apply time — fencing can
outlive the request by days — and a denied re-check returns the process to the stage it left
(`resumeWhereLeft` is the enclosing stage's loop task, resolved per stage). The terminal-event
case (`OrderCancelled`, `OrderExpired`, `OrderRejected`) is the same path entered through
`terminate-on-terminal-event` in fragment (f).

#### (e) Hold and resume

**ID**: `cpt-cf-bss-orders-workflow-seq-def-hold-resume`

**Use cases**: `cpt-cf-bss-orders-workflow-fr-owf-hold-resume`

**Actors**: `cpt-cf-bss-orders-workflow-actor-owf-orders-lifecycle`, `cpt-cf-bss-orders-workflow-actor-owf-generic-approval`

```yaml
# the hold arm, placed INSIDE the competing fork whose escalation `wait` a hold must pause,
# and inside the other stage forks so a hold is always consumable; the lifetime arm is OUTSIDE all of them
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
          export: { as: '${ $context + { holdEventId: .id } }' }
      - applyHold:                                  # protected (08): phase → suspended; computes and records the remaining escalation window from the gate's opened_at and window
          step: apply-hold                          # body: ref + holdEventId, gateRefs; output: escalationRemaining (duration), suspensionRef
          export: { as: '${ $context + { escalationRemaining: .escalationRemaining, suspensionRef: .suspensionRef } }' }
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
                    - arm: { set: { arm: resume } }
              - amendment: { do: [ ...fragment (f) listen, { arm: { set: { arm: superseded } } } ] }
              - cancel:    { do: [ ...cancel arm, { arm: { set: { arm: cancel } } } ] }
      - afterResumeRace:
          switch:
            - resume:     { when: '${ .arm == "resume" }', then: applyResume }
            - superseded: { when: '${ .arm == "superseded" }', then: supersede }
            - cancel:     { then: cancelPath }                    # a cancellation taken from hold: suspended → compensating
      - applyResume:                                # protected (08): phase → started; re-reads drafts (05 rebuild inside) and returns the remainder to re-arm
          step: apply-resume                        # body: ref + suspensionRef; output: escalationRemaining
          export: { as: '${ $context + { escalationRemaining: .escalationRemaining } }' }
```

**Description**: The fragment reproduces [`08 §3.6` *Hold Then Resume with Remaining-Window
Timer Preservation*](./08-hold-and-cancel.md#36-interactions--sequences) as a definition pattern.
**Only the approval-escalation wait pauses on hold**: the hold arm sits inside the `gateLoop`
fork, so the hold branch winning cancels the `escalation` branch's `wait`; `apply-hold` records
the remainder Orders computes; after `apply-resume` the sibling `switch` re-enters `gateLoop` with
`escalationRemaining` re-armed to that remainder. **The lifetime ceiling, the barrier and the
overdue window keep running**: the lifetime `wait` is the top-level competing arm of fragment (a),
outside every stage fork; the barrier's `waitExpected` and the overdue `waitOverdue` are in the
`waves`/`overdue` branches, which a hold arm inside the barrier's *inner* poll fork does not
cancel — the inner fork's hold branch returns to `evaluate` with `arm: poll`, and
`evaluate-activation-eligibility` answers `released: false` while the instance is `suspended`
because the dispatch operations read `owf_process_instance.suspended`
([`01 §3.7`](./01-foundation.md#table-owf_process_instance)). Accepted intents run to their
terminal outcome inside Subscriptions and are recorded by Orders' consumer during the hold; the
draft auto-void TTL is not paused and the rebuild is `apply-resume`'s (slice 05). Whether the DSL
accepts the remainder as a runtime-expression `wait` duration is **Q-11** (§4); if it does not,
the re-arm is a registered Function that sleeps the remainder, which is the one place a
Function would enter these definitions.

#### (f) Amendment and terminal order events

**ID**: `cpt-cf-bss-orders-workflow-seq-def-amendment-and-terminal`

**Use cases**: `cpt-cf-bss-orders-workflow-fr-owf-start-contract`, `cpt-cf-bss-orders-workflow-fr-owf-terminal-order-events`, `cpt-cf-bss-orders-workflow-fr-owf-approval-request`

**Actors**: `cpt-cf-bss-orders-workflow-actor-owf-orders-lifecycle`

```yaml
# the amendment / terminal-event arm of every competing fork:
- amendment:
    do:
      - awaitLifecycle:
          listen:
            to:
              any:
                - with: { type: gts.cf.core.events.event.v1~cf.bss.orders.event.v1~cf.bss.orders.amended.v1~ }
                  correlate: { orderId: { from: '${ .data.orderId }', expect: '${ $context.orderId }' } }   # a NEWER version: no orderVersion correlation
                - with: { type: gts.cf.core.events.event.v1~cf.bss.orders.event.v1~cf.bss.orders.cancelled.v1~ }
                  correlate: { orderId: { from: '${ .data.orderId }', expect: '${ $context.orderId }' }, orderVersion: { from: '${ .data.orderVersion }', expect: '${ $context.orderVersion }' } }
                - with: { type: gts.cf.core.events.event.v1~cf.bss.orders.event.v1~cf.bss.orders.expired.v1~ }
                  correlate: { orderId: { from: '${ .data.orderId }', expect: '${ $context.orderId }' }, orderVersion: { from: '${ .data.orderVersion }', expect: '${ $context.orderVersion }' } }
                - with: { type: gts.cf.core.events.event.v1~cf.bss.orders.event.v1~cf.bss.orders.rejected.v1~ }
                  correlate: { orderId: { from: '${ .data.orderId }', expect: '${ $context.orderId }' }, orderVersion: { from: '${ .data.orderVersion }', expect: '${ $context.orderVersion }' } }
          export: { as: '${ $context + { lifecycleEventId: .id, lifecycleEventType: .type, newOrderVersion: .data.orderVersion } }' }
      - arm: { set: { arm: superseded } }
# the path the sibling switch routes to:
- supersede:
    do:
      - classify:
          switch:
            - amended:  { when: '${ $context.lifecycleEventType | endswith("amended.v1~") }', then: admitSupersession }
            - terminal: { then: terminalEvent }
      - admitSupersession:                          # protected (02): dedup + version comparison; a trigger for a superseded version is ignored, a newer one supersedes
          step: admit-trigger                       # body: ref + triggerEventId: lifecycleEventId; output: admission ∈ supersede | ignore
          export: { as: '${ $context + { admission: .admission } }' }
      - onSupersession:
          switch:
            - ignore:    { when: '${ $context.admission == "ignore" }', then: resumeWhereLeft }
            - supersede: { then: fenceForSupersede }
      - fenceForSupersede: { step: run-cancellation-fence }     # protected (06); trigger: supersede — cancels open gates, voids un-activated wave-1 drafts, compensates activated
      - compensateForSupersede: { then: compensateOrder }        # fragment (c); report-outcome outcome: superseded; terminate-instance terminationKind: superseded, supersededByOrderVersion
      - terminalEvent:                              # protected (02): OrderCancelled | OrderExpired | OrderRejected for an active instance
          step: terminate-on-terminal-event         # output: terminate (bool)
      - fenceForTerminal: { step: run-cancellation-fence }      # protected (06); trigger: terminal-event
      - compensateForTerminal: { then: compensateOrder }         # fragment (c); report-outcome is a no-op for a terminal order (Lifecycle already terminal); terminate-instance terminationKind: terminal-order-event
```

**The new version's invocation.** `OrderAmended` is also the platform event trigger's concern:
the trigger of §3.3 binds `OrderSubmitted` only, and the amended order's new version reaches the
process as its own `OrderSubmitted`-equivalent start per the Lifecycle amendment contract
([`02 §3.3`](./02-triggers-and-start.md#33-api-contracts) routing table). The old invocation
terminates through `supersede`; the new invocation starts through `admit-trigger` under the new
`orderVersion`, whose `start-instance` succeeds once the old instance's `terminal_outcome` is set
(`01 §3.7` partial unique index). Whether one broker event can both start a new invocation
through a trigger and be consumed by a running invocation's `listen` is not stated in the
serverless-runtime design and is an **upstream ask** (commit D); if it cannot, the new version's
start trigger is `OrderAmended` itself, bound as a second event trigger, and this fragment's
`listen` still supersedes the old one.

**Description**: The fragment reproduces [`02 §3.6` *Terminal-event compensation, void, and
audit*](./02-triggers-and-start.md#36-interactions--sequences) and `02 §4`'s void-on-superseded-
version rule. Termination is symmetric with start: a terminal order event and a superseding
version both run the cancellation fence and the compensation walk before `terminate-instance`,
and neither leaves a wave-1 draft for a platform TTL this gear does not own. The admission
decision — is this event for a superseded, current or newer version — stays in `admit-trigger`,
read against Lifecycle under R1, never in a jq comparison over event data.

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
of a healthy long-running instance, `failed` and `dead_lettered` feed the backstop sweep.

## 4. Definition Normative Rules

### 4.1 The fence

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-constraint-definition-fence`

Every published version **MUST** contain the protected operations of
[`01 §3.3`](./01-foundation.md#the-step-operation-contract) and the slices' §3.3 declarations in
the following order constraints, and the validation hook **MUST** refuse a version that violates
any of them:

| Stage | Protected operations, in order | May be interleaved with (composable) |
|-------|-------------------------------|--------------------------------------|
| Admission | `admit-trigger` **<** `start-instance` | — |
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
`evaluate-payment-auth-eligibility`). The exact `:plugin-control` verb and payload are an
upstream ask (§3.3); this rule binds regardless of their shape.

### 4.5 The hold pattern, and Q-11

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-constraint-definition-hold-pattern`

The escalation `wait` **MUST** be inside a competing `fork` that also contains the hold arm, so
that a hold cancels it; `apply-hold` **MUST** return the remaining window computed from Orders'
record and the definition **MUST** re-arm exactly that remainder after `apply-resume`. The
lifetime `wait` **MUST** be a top-level competing arm outside every stage fork, and the overdue
`wait` **MUST** be in a branch a hold arm does not cancel. **Q-11 is registered open**
(`../DECISIONS.md`, commit D): whether the Serverless Workflow DSL 1.0.0, as the platform's plugin
implements it, (i) accepts a runtime expression as a `wait` duration, so the remainder and the
expected-fulfillment instant can be armed without a Function; (ii) surfaces the Problem body's
`error_code` on `$error` so a `catch` can tell `idempotency-key-conflict` from `still-processing`
before the retry budget is spent; (iii) offers any dynamic parallel construct, so a wave could
fan out per line inside the definition rather than inside the operation; and (iv) treats a
`listen` inside a competing `fork` as cancellable without losing an event delivered during
cancellation. Until Q-11 is answered, (i) falls back to a registered Function that sleeps the
remainder, (ii) is bounded by the retry budget, (iii) is settled as one `call` per wave, and (iv)
is covered by the poll arm of the barrier and the re-entry of every stage loop.

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
