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
  - [4.2 The validation hook contract and the publish job](#42-the-validation-hook-contract-and-the-publish-job)
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
the order of the `composable` arms, which manual-task or escalation arm an answer routes to, how
often a stored deadline is re-checked, how many times a transient failure is retried — is
process shape; it changes when the business changes its mind, and it should not need a Rust
release to do so. How long an approval gate waits before escalating, the overdue window and the
manual-task SLA classes are neither: they are per-seller policy values pinned on Orders' record,
changed by a policy write (decision D-134). The platform gear `serverless-runtime` exists for exactly that split: it
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
through the platform registry; changing what a step does is an Orders release; §4.7 is the one
table of which change needs which (decision D-136).

**What crosses the boundary is references.** A definition task carries only members of the
closed vocabulary of [`../ADR/0013`](../ADR/0013-cpt-cf-bss-orders-workflow-adr-references-not-payloads.md)
as amended by D-131: identities (`correlationId`, `orderId`, `orderVersion`,
`supersededByOrderVersion`, `resourceTenantId`, the platform `invocationId` and `attemptId`, the
binding members, consumed-event ids; the full list is ADR-0013 item 1), opaque record references (`gateRef`, `taskRef`,
`lineRef`, `stepRef`, `planRef`, `parkRef`, `requestRef`, `suspensionRef` and their arrays),
counters, closed enums an operation returned, instants and durations, and `stepsBase`. It never
carries a resolved total, an approver identity, a seller or payer tenant axis, a justification or
a downstream payload. That keeps commercial **content** out of task data by construction, and it
keeps audit independent of engine purge by construction. It does not by itself satisfy the PRD §15
Q-01 criteria. History still holds the identifiers (the resource tenant among them), the counters
and cardinalities, and the consumed events as published until the member-storage or thin-event ask
lands, and the isolation, retention and residency of that history are Q-12, pending on the
platform.

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
| `cpt-cf-bss-orders-workflow-fr-owf-approval-request`, `…-fr-owf-approval-escalation`, `…-fr-owf-approval-decision` | Path (a): `obtain-verdict` → `reflect-verdict` → `open-gates` → a competing `fork` of the decision `listen`, the escalation re-check tick and the hold arm; the park loop on an unobtainable verdict. |
| `cpt-cf-bss-orders-workflow-fr-owf-payment-auth`, `…-fr-owf-fulfillment-plan`, `…-fr-owf-provisioning-intent` | Path (b): `evaluate-payment-auth-eligibility` with a `listen` for `OrderAcceptanceRecorded`, `construct-and-freeze-plan`, `begin-fulfillment`, the two waves and the barrier as an explicitly re-evaluated conjunction of the expected-time re-check and the all-creates condition, both answered by `evaluate-activation-eligibility` from Orders' record. |
| `cpt-cf-bss-orders-workflow-fr-owf-retry`, `…-fr-owf-manual-task`, `…-fr-owf-compensation-execution` | Path (c): task retry policy on the dispatch calls, `try`/`catch` around them, `create-manual-task`, a `listen` for the resolution and either resume or `compensate-order` → `report-outcome`. |
| `cpt-cf-bss-orders-workflow-fr-owf-terminal-order-events` | Path (d) for an authorised cancel and the terminal-event `listen` arm of path (f): `authorize-cancel` / `terminate-on-terminal-event` → `run-cancellation-fence` → `compensate-order` → `report-outcome` → `terminate-instance`. |
| `cpt-cf-bss-orders-workflow-fr-owf-hold-resume` | Path (e): hold and resume as `listen` arms; the hold wins the gate loop's race, `apply-hold` pauses the gate window in Orders' record and `apply-resume` re-bases it and answers the first `due`; the lifetime ceiling as a top-level competing `P90D` `wait` that no hold cancels. |
| `cpt-cf-bss-orders-workflow-fr-owf-overdue-escalation` | The overdue window as the top-level `overdueMonitor` branch: an hourly re-check through `raise-overdue-escalation`, which is due only past `expected_fulfillment_at` + the overdue window pinned on the plan (default 24 h, D-134) and before a settled outcome, records the escalation once and nothing else, and is never paused by a hold. |
| `cpt-cf-bss-orders-workflow-fr-owf-process-state-nonauth` | Every effect is a step operation that writes Orders' record; the definition holds no state Orders does not also record, and carries references only. |

#### NFR Allocation

| NFR ID | NFR Summary | Allocated To | Design Response | Verification Approach |
|--------|-------------|--------------|-----------------|----------------------|
| `cpt-cf-bss-orders-workflow-nfr-owf-durability` | Zero in-flight workflows lost across restarts | Platform plugin (invocation history) + `01` envelope | The plugin resumes the invocation; every re-issued call is absorbed under the same key | Platform worker kill/restart with the canonical definitions; no duplicate effect, no lost step |
| `cpt-cf-bss-orders-workflow-nfr-owf-escalation-timer` | Per-gate window from the seller's policy, default 72 h, pinned on the gate row (D-134), ± 5 min | Definition re-check loop (plugin durable timer) + `03` stored deadline | The gate loop's fixed `PT30S` `waitProbe` tick re-checks the gate's stored escalation deadline through `escalate-gate` `mode: fire` before it probes (§3.6 *Fixed waits and re-check loops*); between one fire's database read and the next the gate loop runs at most the rest of that fire, the probe, the tick — or `record-decision` when a decision wins the tick — and the next fire, each call under the 60-second `gate` timeout, which its own `gate` retry policy's 55 s worst case fits (4 × 10 s + 3 × 5 s), so the worst case with every call's retries running to its timeout is 30 s + 4 × 60 s = 4 min 30 s, inside ± 5 min (decisions D-148, D-162); a hold pauses the window in Orders' record and `apply-resume` re-bases it | Timer-accuracy test across a plugin worker restart; hold/resume test asserting the remainder |
| `cpt-cf-bss-orders-workflow-nfr-owf-fulfillment-sla` | p95 ≤ 15 min from activation eligibility to terminal outcome | Definition task timeouts and retry policy; `05` admission | Wave-2 task timeout 3 min, which bounds the call's retries; each attempt's deadline is nested inside it by validation (§2.2 rule 4); the barrier releases on the first evaluation after both conjuncts hold | Load test over the canonical definition |
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
Platform registry     POST/GET /api/serverless-runtime/v1/functions — the definition versions;
(host)                Orders' pre-publish validation hook is a pending ask (UPSTREAM_REQS §2.9),
                      the CI test enforces the rules until then (§3.2)
       │
Platform plugin       interprets the bound version: do · call · listen · wait · switch · fork ·
(Temporal)            try/catch/raise · set — durable timers, DSL retry, replay, correlation
       │  HTTP call tasks
       ▼
Orders step surface   POST /bss-orders-workflow/v1/steps/{operation}  (01 §3.3)
       │
       ▼
Orders record         owf_definition_binding pins the version per instance (01 §3.7)
```

| Layer | Responsibility | Technology |
|-------|---------------|------------|
| Definition | The canonical document of §3.6: one workflow, a stage dispatcher over six paths, calling only registered operations | Serverless Workflow DSL 1.0.0, YAML, jq expressions |
| Registry and validation | Draft, validate, publish, list versions; the pre-publish validation hook Orders supplies — a pending ask, with the CI test as the enforcing check until it lands | serverless-runtime Function Registry API ([`DESIGN.md:857`](../../../../serverless-runtime/docs/DESIGN.md#function-registry-api)); the platform has only the plugin's registration-validation hook (`DESIGN.md:762`); `…-upreq-serverless-runtime-definition-versioning-validation-hook` |
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
No `call` targets a registered platform Function, because a Function's effect would be outside
Orders' record and outside the step surface's authorization (decision D-136). It never calls
Orders Lifecycle, Subscriptions, Payments or the Generic Approval service
directly — seam rules R1–R5 bind the operations — and its jq expressions select and re-key
references; they never compute a business value ([`01 §4.14`](./01-foundation.md#414-determinism-discipline-what-is-computed-on-which-side-of-the-boundary)).

**ADRs**: `cpt-cf-bss-orders-workflow-adr-flow-as-platform-definition`

#### Protected steps are ordered, never omitted

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-principle-protected-steps`

An operation registered `protected` ([`01 §3.3`](./01-foundation.md#33-api-contracts)) **MUST**
appear on every path that reaches its stage, in the order constraints of §4 *The fence*; a
definition version that omits one, replaces one with a `composable` operation or a Function, or
wraps one in a `catch` that swallows its failure **MUST** be refused by the validation hook. A
`composable` operation may be omitted or re-positioned inside its stage, except the `p1`
composables §4.1 requires on their paths — `open-gates` and `escalate-gate` on the gate path,
`park` and `arm-park-escalation` on the park path, `raise-overdue-escalation` for the park and
outage escalations, `resolve-manual-task` after every task, `evaluate-activation-eligibility`
before wave 2 — which stay `composable` in the operation registry
and may be re-positioned, not dropped (decision D-135). This is what lets the
flow change without an Orders release while the fence — admission, binding, verdict, plan freeze,
begin-fulfillment, the waves, the spawn signal, the cancellation fence, compensation, outcome
report, termination — stays whole.

**ADRs**: `cpt-cf-bss-orders-workflow-adr-definition-versioning-and-protected-steps`

#### References, not payloads

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-principle-definition-references-only`

The input and output of every `call` validate against the operation's registered GTS reference
schemas; a definition whose task `input`/`output`/`export` names a member outside them is refused
before publish, and a call carrying one is refused at the envelope. `$context` holds only members of
the ADR-0013 vocabulary (as amended by D-131): what an operation returned, routing literals the
definition writes itself, and `stepsBase`. No `set` or `export` reads an `$error` member other than
`status`, and the step route's error answers carry only fixed members (D-132). The platform's
timeline and stored task payloads therefore contain identifiers, counters, cardinalities and enums,
and no commercial content. What remains is the residual of ADR-0013.

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
| `do` | An ordered list of named tasks; a flow directive may target only a task of the same list (dsl.md *Task Flow*) | The stage dispatcher and its stage tasks; every stage is one flat `do` list that ends with `then: exit` back to `dispatch` (§3.6, D-80 as amended) | Sequential activities of the interpreted workflow |
| `call: http` | Perform an HTTP request (`method`, `endpoint`, `headers`, `body`); a non-2xx answer raises a *communication* error carrying the HTTP `status` | Every step operation: `POST /bss-orders-workflow/v1/steps/{operation}` with `Idempotency-Key` derived from task inputs. No `call` targets a registered Function (D-136) | The plugin's activity for outbound HTTP; the 4xx/5xx answer is the `$error` the `catch` sees |
| `listen` | Consume one or more events matching filters (`to.one`, `to.any`, `to.all`), each filter `with` event properties and `correlate`-d on expressions; the output is the array of consumed events, read as `data` unless `read` says otherwise (dsl-reference.md *Listen*) | The eight non-start Lifecycle triggers, the approval decision, Subscriptions confirmations and operator signals, correlated on `orderId` and `orderVersion`; always `read: envelope` with an `output.as` over `.[0]` that keeps reference members only | Temporal signal / event subscription with event-driven continuation (ADR-0004 *Option A*, `DESIGN.md:630`) |
| `wait` | Pause for a duration given as an inline duration object or an ISO 8601 string — never a runtime expression (dsl-reference.md *Wait*) | Fixed ticks of the re-check loops (§3.6 *Fixed waits and re-check loops*) and the literal `P90D` lifetime ceiling | Temporal durable timer; survives worker restart |
| `switch` | Evaluate cases in order; the first `when` that holds (or the default case) selects a `then` | Verdict, reflection, admission, gate state, policy, resolution branching | Deterministic branch inside the interpreted workflow |
| `fork` | Run branches concurrently; `compete: true` completes with the first branch to finish and cancels the others | The top-level `lifetime` fork (process × ceiling × overdue monitor); every stage wait (gate loop, park loop, eligibility, expected time, barrier, task resolution, resume wait, operator wait after the ceiling) | Concurrent branches with cancellation of the losers |
| `try` / `catch` | Run tasks; on an error matching `catch.errors` and `catch.when`, optionally `retry` under a policy, then run `catch.do` | Retry of transient step failures under `use.retries.transient`; classifying a spent budget, a spent timeout or a 409 into a `set` the next `switch` routes | The plugin's interpretation of the DSL `catch.retry`; **not** the platform `RetryPolicy`, which is invocation-level and keyed by SDK error category (`DESIGN.md:354`–`370`) |
| `raise` | Raise an error (`type`, `status`, `title`, `detail`) | The dispatcher's `unknownStage` case: a version defect faults the invocation rather than routing anywhere | Interpreted error; the invocation goes to `failed` |
| `set` | Set data in the task's output | Recording which arm of a `fork` completed, the stage routing (`nextStage`, `stageLoop`, `returnStage`), re-keying references into `$context` (with `export.as`) | Workflow-local data |

Not used: `run` and `emit` (above), `schedule` (the start mechanism is the platform event
trigger, §3.3), and `for` — the spec's `for` iterates **sequentially**, and
`fork` takes a **static** branch list, so the DSL has no per-line dynamic parallel fan-out. Wave
dispatch is therefore **one `call` per wave carrying the line set as references**, and the bounded
per-order parallelism is inside `dispatch-wave1-create` / `dispatch-wave2-activate` under the
admission controls of [`05`](./05-provisioning-intents.md); this is registered under Q-11 (§4).

**Retry policy** is declared once in `use.retries.transient` — `delay` 1 s, `backoff.exponential`,
`jitter` from 0 s to 30 s, `limit.attempt.count` 5 — and attached by `catch.retry` to the `try`
around each call whose operation is `retryable-on: transient`; this is the DSL's per-task retry
(dsl-reference.md *Try*, *Retry*), not the platform's invocation-level `RetryPolicy`
(`DESIGN.md:354`–`370`). The retry condition is `$error.status` in {429, 503, 504, 409}, written as
a set test in `catch.when` —
`'${ $error.status as $s | any((429, 503, 504, 409); . == $s) }'` — over the DSL communication
error type: the envelope answers a transient failure, an open breaker, `still-processing` and
`idempotency-lease-expired` with those statuses
([`01 §3.3`](./01-foundation.md#the-step-operation-contract)). A status the set does not name,
such as 400, is not retried because no `catch` matches it. Where an outer `catch` owns the 409 —
`admit-trigger`'s supersession wait and the waves' read-before-re-issue — the inner `catch` uses
the same test without 409. `begin-fulfillment` owns no 409: its version conflict and its hold
refusal are settled successes (`version-conflict`, `held`), so its every 409 is a same-key
retry (`04 §3.6`). Every task that calls an
operation declares a `timeout` (`use.timeouts`: `step` 3 min, `wave1` 10 min, `wave2` 3 min,
`admission` 25 h, `gate` 60 s) on the `try` that carries its retry. The timeout bounds the retries themselves, so rule 4 checks only that one attempt fits in it; whichever of the timeout and the retry limit is spent first faults the invocation alike (§4.6). A 409 `idempotency-key-conflict`
is a caller defect the same-key retry cannot fix; it exhausts the budget bounded and faults the
invocation, or lands on a named failure route where §4.6 names one. Whether the plugin surfaces the Problem body's `error_code` on
`$error` so the two 409s can be told apart before the budget is spent is part of Q-11. That a `catch` carrying only `retry` re-raises the last error once its limit is spent — the premise of rule 6 and of §4.6 — is not stated by the DSL either (dsl.md *Retries*, dsl-reference.md *Catch*): it is Q-11 (vi), assumed until the plugin answers.

#### The closed trigger set

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-constraint-definition-listen-targets`

A `listen` filter `with.type` **MUST** be one of: the nine Orders Lifecycle state events
(`OrderSubmitted` and `OrderAmended` as the two platform event triggers of §3.3; `OrderApproved`,
`OrderAmended`, `OrderHeld`, `OrderResumed`, `OrderAcceptanceRecorded`, `OrderCancelled`,
`OrderExpired`, `OrderRejected` as `listen` targets), whose GTS identifiers are Lifecycle's
(`gts.cf.core.events.event.v1~cf.bss.orders.event.v1~cf.bss.orders.<name>.v1~`,
[Lifecycle `01 §4.4`](../../../orders-lifecycle/docs/design/01-foundation.md#44-events-audit-and-the-outbox-normative));
the approval decision event of [`03 §3.3`](./03-approval-execution.md#33-api-contracts); the
two Subscriptions outcome events of [`05 §3.3`](./05-provisioning-intents.md#33-api-contracts)
(`ProvisioningIntentConfirmed`, `ProvisioningIntentFailed`); Orders' own
`OrderFulfillmentCompleted` and `OrderFulfillmentAborted` (permitted; the canonical version no longer listens for them,
because its overdue monitor stops with the invocation, §3.6 (b));
and the operator signal types of §3.3 — `cancel-requested` (08), `reauthorize-requested` (04),
`task-resolution-requested` (07) and `unpark-requested` — the last reserved: no version **MAY**
`listen` for it until Q-13 gives it an origin route and a request row, and `unpark` then refuses
without one (decision D-122). Every filter **MUST** `correlate` on
`orderId` and `orderVersion` against `$context`, except the `OrderAmended` `listen`, which
correlates on `orderId` only because the amended version is newer (`02 §4.7` item 8). Every
`listen` that consumes one of the nine Lifecycle triggers **MUST** be followed by `admit-trigger`
with `role: listen` before any consuming operation (`02 §4.7` item 1). Any other type is refused
before publish.

#### Validation before publish

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-constraint-definition-validation-rules`

A definition version **MUST** pass every rule below in the CI test that runs them over the
canonical definitions of §3.6 in this repository **and**, once the platform registry calls a
consumer-supplied hook before publish — a pending ask
(`cpt-cf-bss-orders-workflow-upreq-serverless-runtime-definition-versioning-validation-hook`,
[`../UPSTREAM_REQS.md`](../UPSTREAM_REQS.md) §2.9) — in that hook (§3.2):

1. [ ] - `p1` - Every `protected` operation of each path appears exactly where §4 *The fence* orders it; `settle-from-lookup` and `retry-step`, which run only in-process (`01 §3.3`, D-108), never appear. A path is a walk of the **routing graph**, whose states are a task position plus the values of the routing members `nextStage`, `stageLoop`, `returnStage`, `taskReturnStage`, `taskReturnLoop`, `ceilingReturnStage`, `ceilingReturnLoop`, `ceilingReturnBack`, `heldStage`, `heldLoop` and `arm`. Every write of a routing member **MUST** be a string literal, a copy of another routing member, or an `if … then … else` over routing members that yields one of those — never an operation's output or another `$context` member — so each member ranges over a finite set of literals the check enumerates. Ten **pinned members** are tracked the same way (decisions D-135, D-144): `verdict`, written only as a copy of `obtain-verdict`'s `verdict`; `reflected`, only as a copy of `reflect-verdict`'s `reflected` or the literal `refused` its `catch` sets; `policy`, only as a copy of `construct-and-freeze-plan`'s `policy`, the seller's partial-failure policy pinned at freeze (`04 §2.2`); `forceTask`, only as a boolean literal; `beginResult`, only as a copy of `begin-fulfillment`'s `result`, null until the first call (decision D-143); `released`, only as a copy of `evaluate-activation-eligibility`'s `released`; `spawned`, `planFailed` and `preAdmitted`, only as boolean literals, each `false` where it is initialised or consumed (`preAdmitted` also in every arm that writes `lifecycleEventId`, decision D-161) and `true` only in the task one case of a `switch` over an operation's closed enum routes to (`spawnSent` after a sent spawn signal, `enterPlanFailedWait` after a `planFailFast` that did not begin, a `toLifecycle` after a hold, resume or acceptance admission answered `supersede` or `terminate`); and `failureScope`, only as one of the literals `line`, `plan` and `order` in the task that enters the failure stage. At each write of a pinned member copied from an output the walk forks once per value of its closed enum; a literal write is decided. The canonical version routes every `switch` that leads towards a protected operation, a stage exit or a `p1` composable of §4.1 on routing and pinned members only, or on a member both of whose ways satisfy §4.1 (decision D-144). A `switch` case over routing and pinned members is decided by the state; a case over any other member is taken both ways, and a §4.1 condition written *on some walk* is checked as reachability from the task it names. The check explores every state reachable from `admitTrigger` and refuses the version if any walk to `end` breaks §4.1 or leaves a stage by a `nextStage` no `dispatch` case names; a walk it cannot decide is refused, never assumed (decision D-126) - `inst-def-protected-present`
2. [ ] - `p1` - Every `call: http` targets `POST /bss-orders-workflow/v1/steps/{operation}` with `{operation}` a row of `owf_step_operation`, and its `endpoint` is exactly `${ $context.stepsBase + "/<operation>" }` with the operation name a literal. `stepsBase` is written only by `input.from`; no `set`, `output` or `export` **MAY** name it, so no version can send its calls, or the credential the plugin attaches to them, to another host, and the CI test and the hook compare the `input.from` value with the environment's step-surface base (§3.7). No `call` **MAY** target a registered Function, `composable` operation or not (decision D-136) - `inst-def-call-targets`
3. [ ] - `p1` - Every `listen` filter type is in the closed set above and carries the two correlations, except the `OrderAmended` filter, which correlates on `orderId` only (the closed set above, `02 §4.7` item 8); a filter **MAY** add a correlation, as the ceiling wait's `taskRef` does - `inst-def-listen-targets`
4. [ ] - `p1` - Bounds nest, over values the definition holds: every task that calls an operation declares a `timeout` from `use.timeouts`; the operation's `deadline_ms` **<** that timeout, so one attempt fits; every task timeout and every literal `wait` **<** the literal `P90D` lifetime `wait`. The check computes no cumulative backoff, because the DSL gives `backoff.exponential` no multiplier (dsl-reference.md *Retry*) and the timeout bounds the retries whatever their curve. The one exception is a task under the `gate` timeout, whose timeout is a term of the escalation bound and so must never be what ends its retries: it **MUST** carry the `gate` retry policy, whose backoff is `constant`, and the check computes attempts × the largest `deadline_ms` called + (attempts − 1) × (delay + the jitter maximum) **<** the `gate` timeout — 4 × 10 s + 3 × 5 s = 55 s < 60 s in the canonical (decision D-162). The escalation window, the overdue window and the SLA classes are not definition values — each is a per-seller policy value that an operation pins on the order's record (decision D-134) — so their bounds are checked where the seller's policy is written (`07 §4.8` item 8) and, for the overdue window against the `wave1` timeout, also by the publish job (§4.2 step 1, decision D-159), not here (decision D-126) - `inst-def-bounds-nest`
5. [ ] - `p1` - Every task `input`, `output`, `export` and every `body` member validates against the operation's registered reference schemas; no member outside them. Every member the definition writes into `$context`, including by `set` and `input.from`, is of one of the six vocabulary types of ADR-0013 as amended by D-131 (identity, opaque record reference, counter, closed enum or boolean, instant or duration, `stepsBase`), a string member in one of the formats that ADR lists, including the invocation, attempt and GTS callable identifier formats D-156 added; a string member of no such type or format is refused. No `set`, `output` or `export` **MAY** read an `$error` member other than `status` (`error_code` once Q-11 (ii) answers) - `inst-def-references-only`
6. [ ] - `p1` - No `protected` operation is inside a `try` whose `catch` continues the forward path: a `catch` either only retries, so exhaustion faults the invocation, or routes to one of the named failure routes of §4.6; that a retry-only `catch` re-raises the last error once its limit is spent is Q-11 (vi), assumed until the plugin answers - `inst-def-no-swallowing-catch`
7. [ ] - `p1` - No `run`, no `emit`, no `for`, no `schedule`: the start mechanism is exactly the two platform event triggers of §3.3, `OrderSubmitted` and `OrderAmended` — Lifecycle publishes no `OrderSubmitted` after an amendment ([Lifecycle `04 §4.3`](../../../orders-lifecycle/docs/design/04-versioning.md#43-re-approval-is-a-two-step-seam-interaction-normative), `02 §4.7` item 9); every `wait` is a literal duration (§3.6 *Fixed waits and re-check loops*) - `inst-def-grammar-subset`
8. [ ] - `p1` - Every branch of a competing `fork` that can complete ends by `set`-ting `arm` so the sibling `switch` can route; every `fork` is followed by a `switch` on `arm`; every `then` names a task of its own `do` list, `exit` or `end` (the stage dispatcher of §3.6). Every competing `fork` inside a stage carries the four shared arms of §3.6 — the hold arm and the stage-level resume arm of (e), the lifecycle arm of (f) and the cancel arm of (d) — except the unwind's forks, which carry the cancel arm and no other (`06 §4.7` item 7), the ceiling wait `awaitOperatorAfterPark`, which carries the lifecycle and cancel arms, and the resume wait `awaitResume`, which carries its resume `listen`, the lifecycle and cancel arms. **Every `wait` inside a stage is a branch of such a fork**: a stage **MUST NOT** contain a plain `wait` task, so no stage wait — a deferral, a re-issue after a read, a re-poll — leaves a hold, a resume, a lifecycle event or a cancel to signal retention for its duration; the `wait`s of the top-level `lifetimeCeiling` and `overdueMonitor` branches are outside every stage and are not covered (decision D-142); the top-level `lifetime` fork carries `process` and `lifetimeCeiling` with the literal `P90D` `wait` and **MAY** carry `overdueMonitor` (decision D-135) - `inst-def-fork-routing`

#### Versioning, pinning, publish

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-constraint-definition-versioning`

Definition versions follow the platform's GTS-canonical semver for callables
([`DESIGN.md:614`](../../../../serverless-runtime/docs/DESIGN.md#versioning-model)). A **new
version** is required for any change to the `do` structure, a `wait` value, a retry policy, a
`switch` predicate or a `listen` target; there is no in-place edit of a published version. An
instance is **pinned** at `start-instance` to the version the invocation runs
([`01 §3.7` `owf_definition_binding`](./01-foundation.md#table-owf_definition_binding)) and runs
to termination on it; migration is out of scope (PRD §5.2); a version **MUST NOT** be archived or
deleted while a binding names it; the platform registry's `archived`/`deleted` transitions
(`DESIGN.md:599`–`610`) are to be gated by that check in the validation hook once the platform
offers it (pending ask, §2.2), and until then by the publish job, which never archives or
deletes, and the readiness check of §4.2. **Publish roles**: the
Orders publish job of §4.2 is the only publisher, running under the platform-operator publish role
through the registry's publish operation; no person publishes by hand, and who may merge a change
to `definitions/` is §4.2's (decision D-138). A seller-scoped fragment role is registered as
**Q-10** (`../DECISIONS.md`). **Audit of publishes**: the platform records no publisher today — a
registered callable carries `owner`, `created_at` and `updated_at`
([DESIGN_GTS_SCHEMAS.md](../../../../serverless-runtime/docs/DESIGN_GTS_SCHEMAS.md) line 23), and
the audit of definition changes is unaddressed (NEXT_ADR_SCOPE.md line 26) — so the evidence of a
publish is the publish job's run and the registry's version listing, and
`owf_definition_binding.published_by` stays null until the registry reports a publisher, which is
part of the hook ask (decision D-137).

**What a definition change may and may not do** is the one table of §4.7 (decision D-136).

## 3. Technical Architecture

### 3.1 Domain Model

**Technology**: Serverless Workflow DSL 1.0.0 documents (YAML) held in the platform function
registry; GTS reference schemas from `01 §3.3` for every task boundary.

**Core Entities**:

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-entity-process-definition`

The registered Workflow callable `gts.cf.core.sless.workflow.v1~cf.bss.orders_workflow.order_process.v1~`
(derived from the platform's workflow base type, [`DESIGN.md:279`](../../../../serverless-runtime/docs/DESIGN.md#functions-and-workflows)):
its `implementation` is the declarative `workflow_spec` of §3.6 (`DESIGN.md:388`). Its `traits`
declare all four members the Workflow base type requires
([DESIGN_GTS_SCHEMAS.md](../../../../serverless-runtime/docs/DESIGN_GTS_SCHEMAS.md#workflow-sibling-base-type)
lines 1136–1230), so no member falls to a schema default (decision D-127):

- **`invocation: { supported: [async], default: async }`** — the async-only declaration the
  platform asks of a workflow that suspends or waits for events (`DESIGN.md:653`); a synchronous
  start is refused instead of failing at its first suspension point with `sync_suspension` (409).
  `entrypoint` keeps its default, because the platform does not say whether an event-trigger
  start is an external invocation; direct starts are the ask
  `…-upreq-serverless-runtime-invocation-control-restriction`.
- **`limits: { timeout_seconds: 23328000, max_concurrent: 50000 }`**
  ([`#limits-base`](../../../../serverless-runtime/docs/DESIGN_GTS_SCHEMAS.md#limits-base), defaults
  30 s and 100; the platform defines them only as "max execution duration in seconds" and "max
  concurrent invocations", with no maximum). Both are sized from the residency this document and
  [`../DESIGN.md`](../DESIGN.md) §4.1 state (decision D-157, amending D-127):
  - **`timeout_seconds`** is three `P90D` ceilings, 270 days. The first ceiling is armed only
    after admission, whose timeout is 25 h (`use.timeouts.admission`), so it fires by 25 h + 90 d;
    the fresh ceiling the re-entered fork arms (§3.6 (a), D-121) fires by 25 h + 180 d; the rest,
    about 89 days, carries the second ceiling's park, the operator's action on it and an unwind
    that continues under the next fresh ceiling. The unit is the `P90D` window, not the ceiling
    park: an expiry that `afterLifetime` absorbs through its `unwinding` or `verdictPark` case
    (§3.6 (a)) re-arms the fork without parking and uses up one of the three windows as a ceiling
    does. Two ceilings are reachable on a path that absorbs no expiry. An order whose first window
    expires in the verdict park — unverdicted across day 90 — and which then proceeds reaches its
    first ceiling park at 25 h + 180 d and cannot reach a second, whose window would end after the
    guardrail; that path is the accepted limit (decision D-157 as amended), not a fourth window,
    because it needs a verdict park across day 90, and on it the one ceiling park still escalates
    and raises its task; only what outlives that park is cut by the guardrail and raised as
    `invocation-dead`. A third ceiling is never reachable, so an
    invocation still live at 270 days is ended by the
    platform's duration guardrail (BR-028, [serverless-runtime PRD.md](../../../../serverless-runtime/docs/PRD.md)
    line 483) and the instance liveness pass raises it as `invocation-dead`, whose remedies are
    the platform re-drive and the order cancel (D-105).
  - **`max_concurrent`** is sized for the reading that counts suspended invocations, because a
    cap too low refuses order starts while one too high costs only runaway protection. From
    DESIGN §4.1's figures: the standard path is 5 / s × ~360 s = **1,800**; manual-task waits are
    at most 1 % of lines, so about 3 % of orders at p50 3 lines (1 − 0.99³), each held about 24 h
    (the longer SLA class; a breach escalates rather than ends the wait), 5 × 0.0297 × 86,400 ≈
    **12,830**. Phase 1 therefore needs about **14,630**, since no approval gate fires while
    Generic Approval is inert (Q-05). Each further 1 % of orders held for a 72 h escalation window
    adds 5 × 0.01 × 259,200 = **12,960**, and each 1 % of orders dated *d* days ahead in
    `awaitExpected` adds 4,320 × *d*; the design does not fix either share, which is an input to
    the NFR workshop. 50,000 covers phase 1 with about 35,000 to spare, for example 1 % gated at
    72 h and 1 % dated five days ahead (34,560). If the platform answers that only running
    invocations count, the running population is the calls in flight, about 750 / s at peak × the
    10 s dispatch deadline = 7,500, so 50,000 is only a loose runaway cap and a later version
    **MAY** lower it to 10,000.

  Whether `timeout_seconds` measures wall-clock time including suspension and whether
  `max_concurrent` counts suspended invocations, the platform does not state; both answers, with
  the value to use under each, are a **gating** readiness item of the async-only ask
  ([`../UPSTREAM_REQS.md`](../UPSTREAM_REQS.md) §2.9, (a′)), and so is the platform's capacity for
  them: its stated regional target is "≥ 10,000 concurrent executions per region"
  ([serverless-runtime PRD.md](../../../../serverless-runtime/docs/PRD.md) line 843, BR-208), below
  the phase-1 figure above under the counts-suspended reading.
- **`retry: { max_attempts: 0 }`**
  ([`#retrypolicy`](../../../../serverless-runtime/docs/DESIGN_GTS_SCHEMAS.md#retrypolicy),
  default 3). The platform `RetryPolicy` re-runs a failed invocation by SDK error category
  (`DESIGN.md:354`–`370`); a faulted order process is re-driven only by an operator, through the
  `invocation-dead` task (D-86, D-105), so no automatic invocation retry runs behind it.
- **`workflow`** (`gts.cf.core.sless.workflow_traits.v1~`,
  [DESIGN_GTS_SCHEMAS.md](../../../../serverless-runtime/docs/DESIGN_GTS_SCHEMAS.md#workflowtraits)
  lines 466–530) with its three required members: `compensation: { on_failure: null,
  on_cancel: null }` — **no function-level handler**, because compensation is a path through
  Orders' own operations (§3.6 (c), (d)), never a platform-invoked function that would act outside
  the record; `checkpointing: { strategy: automatic }`; and **`max_suspension_days: 90`**
  (schema default 30, lines 520–529), because a suspension longer than the cap moves the invocation
  `suspended → failed` (`DESIGN.md:455`) and the process may legitimately wait up to its lifetime
  ceiling.

Whether tenant policy may cap `max_suspension_days` below 90, and whether the cap measures one
suspension or accumulated suspended time, are upstream asks
([`../UPSTREAM_REQS.md`](../UPSTREAM_REQS.md) §2.9); with the re-check loops of §3.6 the
invocation wakes at least hourly, so no single suspension approaches the cap. The tenant runtime
policy's quotas `max_execution_duration_seconds`, `max_concurrent_executions` and
`max_execution_history_mb`
([DESIGN_GTS_SCHEMAS.md](../../../../serverless-runtime/docs/DESIGN_GTS_SCHEMAS.md#tenantruntimepolicy)
lines 1824–1855) cap these traits whatever they declare, so the readiness gate checks that the
tenant's values admit them; the history quota is sized against the ask of §3.6 *History growth*.
Its `schema.params` is the reference tuple of the start event. One callable, many versions.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-entity-definition-version`

One published, immutable revision of the process definition: the document of §3.6 at a semver,
whose document `version` equals the registry semver, and its publishing instant (registry-held);
the validation result and the publishing principal are registry-held only once the hook ask lands
(decision D-137).
It is what an instance pins to and what the fence of §4 is checked against.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-entity-definition-task`

One named task of a version: a `call` bound to one registered operation with a derived
idempotency key, or a `listen`, `wait`, `switch`, `fork`, `try`, `raise` or `set` of §2.2. A
`call` task's position (`$task.reference`, dsl.md *Task Descriptor*) plus `$workflow.id` is the
`attemptId` the operation records until the platform supplies its own — the reference, because the
task that evaluates the expression is the inner `call` task every step's `try` wraps, whose name is
the same for every step ([`01 §3.3` *Attempt identity*](./01-foundation.md#33-api-contracts));
that `$workflow.id` equals the platform `invocation_id` is assumed, not stated by the platform,
and is part of the attempt-identity ask (§3.6).

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-entity-process-signal`

An operator-originated instruction delivered to a running invocation and consumed by a `listen`
arm: `cancel-requested` (an authorised workflow-mediated cancel, [`09 §3.3`](./09-read-and-authz.md#33-api-contracts)),
`reauthorize-requested` (an operator's payment re-authorisation, [`04`](./04-fulfillment-plan.md);
origin route pending, Q-13),
`task-resolution-requested` (an operator's `retry`, `override` or `cancel` of a manual task,
[`07 §3.3`](./07-manual-tasks.md#33-api-contracts)) and `unpark-requested` (after a
lifetime-ceiling park; reserved: no origin route and no arm in the canonical version, Q-13,
D-122). Hold
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
    AUTH[Orders publish job, platform-operator role §4.2]
    REG[serverless-runtime Function Registry<br/>/api/serverless-runtime/v1/functions]
    HOOK[Orders validation hook<br/>rules of §2.2 — pending ask; CI test until then]
    OPR[owf_step_operation<br/>01 §3.7]
    TRG[Event triggers: OrderSubmitted, OrderAmended → order_process]
    PLG[Temporal plugin: running invocation]
    STEPS[Orders step surface 01 §3.3]
    SIG[Orders control gateway 09 §3.3 → invocations/{id}:plugin-control]
    AUTH -->|register draft, validate, publish| REG
    REG -.->|consumer pre-publish hook: pending ask| HOOK
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
published — once the platform calls it; today it runs only as the CI test (below).

##### Responsibility scope

Implementing the rules of §2.2 *Validation before publish* over a candidate version: walking the
`do` tree, resolving every `call` endpoint to an `owf_step_operation` row, checking the fence
order of §4 over the routing graph of rule 1, the closed `listen` set, the bound nesting against `deadline_ms`, the reference-only
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

Delivering the request row that an authorised control operation of
[`09 §3.3`](./09-read-and-authz.md#33-api-contracts) has already recorded — 09's gateway is the
sole writer of that row — as a signal to the instance's `invocation_id` (`01 §3.7`), carrying the
reference tuple and the signal type of §3.3; reporting each delivery outcome through 09's
request-delivery port, the one write path to `delivery_state`, so a signal the platform loses is
visible as an unanswered request; retrying delivery under the caller-side duplicate protocol. It
records no row of its own.

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
| Register draft, validate, publish, list versions, deprecate | `/api/serverless-runtime/v1/functions` (CRUD over Function and Workflow entities) | `DESIGN.md:857` | Publishing a definition version from the publish job of §4.2 after the CI test of §3.2 and the behavioural gate; deprecating the version it replaces (D-138); the validation hook would run inside "validate" and "publish" once the platform calls a consumer hook (pending ask); `owf_definition_binding` is to block archive/delete of a bound version through that hook, and until then through the pipeline |
| Start an invocation | `POST /api/serverless-runtime/v1/invocations` with `function_id`, `mode: async`, `params`, `Idempotency-Key` | `DESIGN.md:865`, `DESIGN.md:895`–`910` | Not called by Orders — the two event triggers are the only start of `order_process` (`02 §2.2`, D-107), and a direct start by another caller is asked to be refused platform-side (`…-upreq-serverless-runtime-invocation-control-restriction`); one that happens anyway meets `admit-trigger`'s tenant, category and state checks. An operator re-drive is `…:control` `retry` keeping `invocation_id` (D-86), never a second start; with no function-level handler a failure goes on to `dead_lettered` (`DESIGN.md:458`), from which `retry` is not offered (`DESIGN.md:888`), and "retry from `dead_lettered`, keeping `invocation_id`, resuming at the faulted task" is part of the signals ask; until it is confirmed, the fallback is the dead-instance unwind of `01 §4.16` and a new order (D-105) |
| Read invocation status | `GET /api/serverless-runtime/v1/invocations/{invocation_id}` | `DESIGN.md:867` | The instance liveness pass of the `reconciliation-sweep` worker, which raises an instance whose invocation is not live as an `invocation-dead` task (`01 §3.8`, D-105); `start-instance`, which records the `function_id` and `function_version` the invocation record reports as the binding (`01 §3.3`, D-137); and the progress read (`09`) |
| Generic control | `POST …/invocations/{invocation_id}:control` (`cancel`, `suspend`, `resume`, `retry`, `replay`) | `DESIGN.md:868`, `DESIGN.md:883`–`889` | `retry` by an operator re-drive only, through the `invocation-dead` task (`07 §4.4`), from the states the platform offers it from (`failed` today, `DESIGN.md:888`); `cancel` **never** for an order cancel (§3.2); `suspend`/`resume` **never** — hold is a definition arm, not a platform suspension. Orders cannot stop another caller issuing them: a generic `cancel` surfaces as `canceled` and is raised by the liveness pass, a generic `suspend` only when it times out into `failed`; denying them on `order_process` is `…-upreq-serverless-runtime-invocation-control-restriction` (§4.4) |
| Plugin control (signals) | `POST …/invocations/{invocation_id}:plugin-control` | `DESIGN.md:869`, `DESIGN.md:893` | Delivery of `cancel-requested`, `reauthorize-requested`, `task-resolution-requested` and `unpark-requested` to the running invocation's `listen` arms |
| Event trigger binding | `/api/serverless-runtime/v1/event-triggers` (create, enable, disable, metrics) | `DESIGN.md:980`–`987` | Two trigger bindings to `order_process`, the one start mechanism (the document declares no `schedule`), declared in the repository's `definitions/` beside the definition and applied only by the publish job of §4.2 (§3.8, D-107, D-138) — `OrderSubmitted`, filtered to `category = new_sale` (a routing filter; `admit-trigger` re-checks the category and the tenant from the Lifecycle read, `02 §3.6`), and `OrderAmended` (`02 §2.2`) — each with `callable_type: workflow` and `execution_context: system`, the platform identity ([DESIGN_GTS_SCHEMAS.md](../../../../serverless-runtime/docs/DESIGN_GTS_SCHEMAS.md) line 1697), because an `event_source` identity would be Lifecycle's producer principal, which the step route refuses; their dead-letter handling is the trigger's `dead_letter_queue` (line 1651; its management API is out of the platform's scope) (`01 §4.8`) |
| Timeline (debug) | `GET …/invocations/{invocation_id}/timeline` | `DESIGN.md:1061` | Operator debugging only; never an Orders read path |

**Signals.** The operator-facing instructions map as follows:

| Instruction | Origin | Delivery | Definition arm | Operation that records it |
|-------------|--------|----------|----------------|---------------------------|
| hold | Lifecycle `OrderHeld` | Broker event, correlated `listen` | §3.6 (e) | `apply-hold` (08) |
| resume | Lifecycle `OrderResumed` | Broker event, correlated `listen` | §3.6 (e) | `apply-resume` (08) |
| cancel | Seller operator, `POST /bss-orders-workflow/v1/workflows/{orderId}/cancel` (`09 §3.3`) | `…:plugin-control` signal `cancel-requested` | §3.6 (d) | `authorize-cancel` (08) |
| re-authorise | Operator, through a control route that is pending (`09 §3.3`, Q-13); a Payments push would be delivered as the same signal only if Payments adds one (`04 §3.3`) | `…:plugin-control` signal `reauthorize-requested` | §3.6 (b) | `evaluate-payment-auth-eligibility` (04) |
| resolve a manual task | Fulfillment or Seller operator `retry` / `override` / `cancel` (`07 §3.3`) | `…:plugin-control` signal `task-resolution-requested` | §3.6 (c) | `resolve-manual-task` (07) |
| unpark after the lifetime ceiling | An operator `retry` of that ceiling's `lifetime-ceiling-reached` task (`07 §3.3`); the reserved `unpark-requested` signal has no origin route and no arm (Q-13, D-122) | `…:plugin-control` signal `task-resolution-requested`, correlated on the ceiling's `taskRef` | §3.6 (d) | `resolve-manual-task` (07), then `unpark` (01), which refuses without that recorded retry |

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
shown separately for review. The fragments are **abridged**: they are not the published bytes
and are not by themselves a valid document. The complete canonical document is the repository
file `definitions/order-process.yaml` (§3.7), and **that** file is the CI artefact that must
validate against the Serverless Workflow DSL 1.0.0 schema (dsl-reference.md, *Workflow*) and
against the rules of §2.2. The abridgements are exactly these, and nothing else is elided:

- A placeholder in guillemets — `‹lifecycle arm of (f)›`, `‹approval stage, below›`,
  `‹abridged call›` — stands for the construct it names, written out in full where named.
- A task written `step: <operation>` stands for the `call: http` task shown in full as `admitStart`
  in fragment (a): `method: post`, `endpoint` `$context.stepsBase + "/<operation>"`, the
  `Idempotency-Key` header derived from the task inputs, and a `body` of `ref` plus the members
  named in the comment. Its `try`, `catch` and `timeout` are **never** elided: where they are not
  written, the task has none.
- `ref` is the reference tuple every body carries — `{ correlationId, orderId, orderVersion,
  resourceTenantId, invocationId: $workflow.id, attemptId: ($workflow.id + ":" + $task.reference) }`
  — `$task` is the inner `call` task, so `$task.name` is `call` for every step while
  `$task.reference` (`/do/…/admitStart/try/0/call`) names the step (dsl.md *Task Descriptor*).
  **`$workflow.id` is assumed to equal the platform `invocation_id`**: the DSL defines it only as
  "a unique id of the workflow execution" (dsl.md, *Workflow Descriptor*) and the serverless-runtime
  design does not state the equality; it is part of the attempt-identity ask
  ([`../UPSTREAM_REQS.md`](../UPSTREAM_REQS.md) §2.9 *Attempt identity and deadline propagation on
  every call*).
- A `set` shown in a fragment is also exported into `$context` with
  `export: { as: '${ $context + . }' }`, which the complete document writes out.
- `catch: *transient` and `catch: *transientNo409` are YAML aliases of the two anchors written
  out in fragment (a); the complete document is one YAML stream, so the aliases resolve there.

**The stage dispatcher** (D-80 as amended: a 1.0.0 flow directive may target only a task in its
own `do` list, dsl.md *Task Flow*). The dispatcher's level is the `process` branch of the
top-level `lifetime` fork rather than the document's own `do` list, because the lifetime ceiling
must compete with every stage; that branch holds
one composite task per stage — `approval`, `fulfillment`, `failure`, `unwind`, `cancel`,
`ceiling`, `hold`, `resume`, `lifecycle` — and a `dispatch` switch on `$context.nextStage` whose
cases `then:` to those siblings. Every stage task carries `then: dispatch`. Inside a stage the
tasks form **one flat `do` list** and jump only to siblings in it; a stage ends by `set`-ting
`$context.nextStage` and `then: exit`, which halts the stage's list (dsl-reference.md, *Flow
Directive*), completes the stage task and returns through its `then: dispatch`. Every stage opens
with an `enter` switch on `$context.stageLoop` that re-enters the checkpoint the stage last
recorded, and otherwise starts at its first task. A path that ends the process ends the workflow
with `then: end` after `terminate-instance`. No `then:` in the canonical definition names a task
at another depth.

**Listen output** (dsl-reference.md, *Listen*: a `listen` produces "a sequentially ordered array
of all the events it has consumed", reading `data` by default). Every `listen` below sets
`read: envelope` and an `output.as` that reads the first consumed event as `.[0]` — `.[0].id`,
`.[0].type`, `.[0].data.<member>` — and selects **only the reference members** the arm needs; the
arm's `set` reads those. The `correlate.from` expressions evaluate on the filtered event itself and
keep `.data.<member>`. The same selection applies to the start trigger's input in `input.from`.
Whether the plugin persists only the selected members, or the raw event before selection, is not
stated by the platform: it is the ask
`cpt-cf-bss-orders-workflow-upreq-serverless-runtime-consumed-event-member-storage`, and the
alternative route is Lifecycle's thin events, `cpt-cf-bss-orders-workflow-upreq-lifecycle-thin-events`
([`../UPSTREAM_REQS.md`](../UPSTREAM_REQS.md) §2.9, §2.4); until one lands the consumed events
sit in engine history as published, the residual of `../ADR/0013` *Trigger inputs and consumed
events*.

**Retry and timeout.** `catch: *transient` retries under `use.retries.transient` when
`$error.status` is in {429, 503, 504, 409}; `catch: *transientNo409` is the same set without 409,
used where an outer `catch` owns the 409 (§2.2); `catch: *gateTransient` is `*transient`'s set
under `use.retries.gate`, on the four gate-loop calls `recordDecision`, `escalateGate`,
`probeGate` and `probeOutage` (decision D-162). Every task that calls an operation declares a
`timeout` from `use.timeouts` — `step` (3 min), `wave1` (10 min), `wave2` (3 min), `admission`
(25 h), `gate` (60 s) — on the `try` that carries its retry, so it bounds the call's retries (§2.2 rule 4). A
timeout raises the DSL's timeout error, whose `type` is `…/errors/timeout` and whose `status`
should be 408 (dsl.md *Timeouts*); it is caught only where an outer `catch` names 408 **and**
carries no `errors.with.type` filter, since a filter on the communication type drops it before
`when` runs, and otherwise faults the invocation.

**Two routing conventions keep the arms honest.** (1) Every branch of a **stage** `fork` only
listens or waits and then `set`s `arm` (and the references the arm needs); no step operation runs
inside a competing stage branch, so a losing branch is never cancelled halfway through an
operation, and the sibling `switch` routes the winner to the task that calls the operation. The
two top-level branches beside `process` are the exception by construction: `lifetimeCeiling`
only waits, and `overdueMonitor` calls one `composable` operation whose answer routes nothing, so
cancelling it in flight loses nothing the envelope has not recorded. (2) The arm values of the
shared arms are stage names — `hold`, `resume`, `lifecycle`, `cancel` — and every stage routes
them to its own `leave` task, `{ set: { nextStage: '${ .arm }', returnStage: <this stage> },
then: exit }`; the shared stage ends in `back`, `{ set: { nextStage: '${ $context.returnStage }' },
then: exit }`, and the `enter` switch of the returning stage re-enters `$context.stageLoop`.
Re-entry at a checkpoint re-issues the checkpoint's calls under their unchanged idempotency keys,
which the envelope answers with the stored outcome (`01 §3.3`). A call that consumes an arm's
payload after the fork — `record-decision` after the decision arm, `resolve-manual-task` after a
task-resolution arm — is a checkpoint of its own: the stage records it in `stageLoop` before the
call and its `enter` switch re-issues it, so a lifetime ceiling that cancels `process` during the
call neither drops the consumed decision or resolution nor waits for a signal that will not come
again (decision D-147).

#### Fixed waits and re-check loops

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-constraint-definition-fixed-waits`

A 1.0.0 `wait` accepts only an inline duration object or an ISO 8601 duration string, not a
runtime expression (dsl-reference.md, *Wait*; the `duration` definition of the 1.0.0 schema). A
deadline Orders computes is therefore a **bounded re-check loop**: a `wait` of the fixed
granularity below, then a `call` to the Orders operation that owns the stored deadline — which
compares database time with it and answers `due: true|false` — and a `switch` that loops while
`due` is `false`, passing each answer's next round into the next call. No Function is used as a sleeper: serverless-runtime Functions are "bounded by
platform timeout limits" and durable waits belong to Workflows
([serverless-runtime DESIGN](../../../../serverless-runtime/docs/DESIGN.md#function-base-type) `DESIGN.md:579`,
`DESIGN.md:582`). A `due: false` answer from `arm-park-escalation`, `escalate-gate` `mode: fire`
and `raise-overdue-escalation` records nothing and is a **settled success of its round** that
returns the next round, the rule `obtain-verdict`'s `unobtainable` and `resolve-manual-task`'s
`none` follow too ([`01 §3.3` *Rounds and attempts*](./01-foundation.md#rounds-and-attempts-the-one-rule-for-re-invokable-operations)): each tick calls a new key, so no loop keeps one key open
towards the 30-day key lifetime, however far away its deadline is. The granularity is part of the definition version (§2.2 *Versioning*); the
deadline stays the owning slice's stored value, and where it derives from a business window —
the escalation window, the overdue window, an SLA class — the window is the seller's policy value
the operation pinned on the record, so no definition version can move it and a policy write moves
it only for records pinned after the write (decision D-134).

| Wait (task name) | Fragment | Fixed granularity | Re-check operation (owner) | Answer the switch reads |
|------------------|----------|-------------------|----------------------------|-------------------------|
| `waitCeiling` — lifetime ceiling | (a) | `P90D`, literal; no re-check | — | — |
| `waitTtlMargin` — park escalation, also the verdict retry interval | (a) | `PT5M` | `arm-park-escalation` (03) | `due` |
| `waitHeldReflect` — reflection refused while the order is held, or answered `moved` | (a) | `PT5M`; the resume arm re-enters at once | `reflect-verdict` (03), under the next round | `reflected` |
| `waitProbe` in `gateLoop` — approval escalation window, then the approval-service probe | (a) | `PT30S` (`03 §4.5` items 4, 5) — worst-case escalation lateness 30 s + 4 × 60 s = 4 min 30 s, inside the ± 5 min of `nfr-owf-escalation-timer`: the rest of a fire, the probe or `record-decision`, the tick and the next fire, each under the 60-second `gate` timeout, which the `gate` retry policy's 55 s worst case fits; every return into the loop — from another stage, after a `pending` decision, from the outage arm — fires first (D-123, D-148, D-162) | `escalate-gate` `mode: fire`, then `mode: probe` (03) | `due` (fire, routes nothing), then `serviceState` |
| `waitProbe` in `outageArm` — approval-service probe and the outage threshold | (a) | `PT30S` (`03 §4.5` item 5) | `escalate-gate` `mode: probe` (03) | `serviceState`; `due` on `outage` |
| resumed escalation window | (e) | none of its own: the first answer is `apply-resume`'s | `apply-resume` (08), then `waitProbe` | `due` |
| `waitEligibility` — eligibility poll | (b) | `PT5M` (`04 §4.8` item 5) | `evaluate-payment-auth-eligibility` (04) | `eligibility` |
| `waitDeferral1`, `waitDeferral2` — wave-1 and wave-2 deferral, the tick branches of `awaitDeferral1` and `awaitDeferral2` (D-142) | (b) | `PT1M` | `dispatch-wave1-create` / `dispatch-wave2-activate` (05), under the next `dispatchRound` | `due` on a non-empty `deferred[]` |
| `waitReread1` — wave-1 re-issue after a 409 and its read, the tick branch of `awaitReread1` (D-142) | (b) | `PT30S`, the barrier's poll interval | `dispatch-wave1-create` (05), under the unchanged key (D-120) | `waveOutcome` |
| `waitExpected` — the barrier's timer half | (b) | `PT1H` | `evaluate-activation-eligibility` (04) | `due` |
| `waitPoll` — barrier confirmation poll | (b) | `PT30S` | `reconcile-intent` (05), then `evaluate-activation-eligibility` | `released` |
| `waitOverdue` — overdue window (`expected_fulfillment_at` + the plan's pinned overdue window, default 24 h, D-134) | (b) | `PT1H` | `raise-overdue-escalation` `overdue-fulfillment` (07), under the next round | `due`, `raised` |
| `waitHeld` — spawn signal or completion report refused while the order is held, or a re-check answered `not-dispatchable` (stage loop `heldSpawn` or `heldReport`, D-145) | (b) | `PT5M`; the resume arm re-enters at once | `re-check-pre-activation` (04) then `report-spawn-signal` (05), or `report-outcome` (06), under the next round | `preActivation`, `spawnSignal`, `lifecycleCall` |
| `waitSla` — manual-task SLA | (c) | `PT5M` | `resolve-manual-task` `trigger: sla-check` (07) | `resolution` |
| `waitSla` in `awaitOperatorAfterPark` — the ceiling task's SLA | (d) | `PT5M` | `resolve-manual-task` `trigger: sla-check` scoped to `ceilingTaskRef` (07), under that task's own `slaRound` (D-129) | `resolution` — `escalated` or `none` only: an order-scope task escalates and is never exhausted (`07 §4.2`) |
| `waitResumePoll` — the resume wait's read of the hold | (e) | `PT15M` | `apply-resume` `trigger: poll` (08), under the next `resumePollRound` (D-130) | `resumeOutcome` |
| held poll — every other wait while a suspension is recorded (`pollHeld`) | (a), (b) | none of its own: it rides the wait's tick, counted in `heldTicks` — every 30th `PT30S` tick of `waitPoll`, every 3rd `PT5M` tick (`waitTtlMargin`, `waitHeldReflect`, `waitEligibility`, `waitHeld`), every `PT1H` tick (`waitExpected`), every 15th `PT1M` tick of a `held` deferral (`waitDeferral1`, `waitDeferral2`), about `PT15M` (`PT1H` in `awaitExpected`), and once on leaving the park loop. `gateLoop` and `outageArm` do not poll: a hold there enters the resume wait, and their `PT30S` tick keeps the escalation fire's lateness bound of D-123 (D-133) | `apply-resume` `trigger: poll` (08), under the next `resumePollRound`, then the tick's own re-check | `resumeOutcome`, `failedTaskRefs` |
| `waitRepoll`, `waitRetryLeg` — compensation re-poll (the tick branch of `awaitRepoll`, D-142) and leg retry | (c) | `PT30S`, `PT1H` | `compensate-order` (06) | `compensationState` |

Q-11 (i) now asks only whether the plugin accepts a runtime-expression duration as an extension
(§4.5); until it is answered, and after it unless a new version says otherwise, the re-check loop
applies.

**History growth.** Every tick is engine history: a `wait`, a competing `fork` of up to seven
branches torn down and re-armed, and one or two activity calls. Per waiting instance and day, the
`PT30S` ticks of `gateLoop` and of the barrier's `waitPoll` add 2,880 iterations each, a `PT5M`
tick 288, `waitResumePoll` 96, the held poll at most 96 while a suspension is recorded (it replaces a tick's re-check, so it adds one call, not one iteration), and the overdue monitor 24 for the whole life of the invocation.
Serverless Workflow DSL 1.0.0 has no construct that truncates history, and the platform states no
per-invocation history bound beyond the tenant quota `max_execution_history_mb`
([DESIGN_GTS_SCHEMAS.md](../../../../serverless-runtime/docs/DESIGN_GTS_SCHEMAS.md#tenantruntimepolicy)
line 1840). The ticks are not lengthened to fit a budget nobody has stated: the gate tick is bound
by the ± 5 min escalation accuracy (D-123) and the barrier poll by the 15-minute fulfillment SLA.
Bounding one invocation's history over a life of 90 days and more is therefore asked of the plugin
(decision D-128, `cpt-cf-bss-orders-workflow-upreq-serverless-runtime-history-growth`,
[`../UPSTREAM_REQS.md`](../UPSTREAM_REQS.md) §2.9), and the platform path is not ready until it
answers.

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
    gate:                               # the gate loop's calls (D-162): worst case 4 attempts × 10 s deadline_ms + 3 gaps × (2 s + 3 s jitter) = 55 s, under the 60 s gate timeout, so the limit, not the timeout, ends the retries
      delay: { seconds: 2 }
      backoff: { constant: {} }
      jitter: { from: { seconds: 0 }, to: { seconds: 3 } }
      limit: { attempt: { count: 4 } }
  timeouts:                             # §2.2 rule 4: each above the deadline_ms of every operation called under it, and below the P90D lifetime wait
    step:      { after: { minutes: 3 } }
    wave1:     { after: { minutes: 10 } }    # 01 §4.2 (D-70)
    wave2:     { after: { minutes: 3 } }     # nfr-owf-fulfillment-sla (D-70)
    admission: { after: { hours: 25 } }      # above the 24 h supersession budget
    gate:      { after: { seconds: 60 } }    # the calls between two escalation fires (D-148): 30 s tick + 4 × 60 s = 4 min 30 s, inside ± 5 min; above the gate retry policy's 55 s worst case (D-162), and so above escalate-gate's and record-decision's 10 s deadline_ms
# no `schedule`: the only start mechanism is the two platform event triggers of §3.3
# triggerKind is looked up from the exact event type (D-107): any other type yields null, which admit-trigger's schema refuses
input:
  from: >-
    ${ { stepsBase: "‹the step surface base URL of this environment, 01 §3.3›",
         orderId: .data.orderId, orderVersion: .data.orderVersion,
         resourceTenantId: .data.resourceTenantId, triggerEventId: .id,
         triggerKind: ({ "gts.cf.core.events.event.v1~cf.bss.orders.event.v1~cf.bss.orders.submitted.v1~": "OrderSubmitted",
                         "gts.cf.core.events.event.v1~cf.bss.orders.event.v1~cf.bss.orders.amended.v1~": "OrderAmended" }[.type]),
         nextStage: "approval", stageLoop: null, returnStage: null, preAdmitted: false,
         verdictRound: 0, reflectRound: 0, attemptKey: null, overdueRound: 0,
         ceilingRound: 0, unwind: null } }
do:
  - admitTrigger:                       # protected (02); role: start
      timeout: admission
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
                      attemptId: ${ $workflow.id + ":" + $task.reference }
            catch: &transientNo409      # trigger-applicability-unverified, breaker, timeouts; the 409 is the outer catch's
              errors: { with: { type: https://serverlessworkflow.io/spec/1.0.0/errors/communication } }
              when: '${ $error.status as $s | any((429, 503, 504); . == $s) }'
              retry: transient
      catch:                            # prior-instance-active (key left open); exhaustion fails the invocation (02 §4.7 item 6)
        errors: { with: { type: https://serverlessworkflow.io/spec/1.0.0/errors/communication, status: 409 } }
        retry: supersession
      export:
        as: '${ $context + { correlationId: .correlationId, admission: .admission } }'
  - onAdmission:                        # 02 §4.7 item 2: only `start` reaches start-instance
      switch:
        - start: { when: '${ $context.admission == "start" }', then: startInstance }
        - other: { then: end }          # absorbed-duplicate | ignored-superseded | ignored-terminated | no-active-instance
  - startInstance:                      # protected (01)
      timeout: step
      try:
        - call: { step: start-instance }   # ‹abridged call›; body: ref + definitionId, definitionVersion: $workflow.definition.document.version (a cross-check: the binding records the function_version of the platform's invocation record, D-137), definitionSource: platform, triggerEventId
      catch: &transient
        errors: { with: { type: https://serverlessworkflow.io/spec/1.0.0/errors/communication } }
        when: '${ $error.status as $s | any((429, 503, 504, 409); . == $s) }'
        retry: transient
      export: { as: '${ $context + { rowVersion: .rowVersion, boundInvocationId: .invocationId } }' }
  - onBinding:
      switch:
        - notMine: { when: '${ $context.boundInvocationId != $workflow.id }', then: end }   # 01 §3.3: start-instance answers the bound invocationId; another invocation's id means a duplicate, which ends
        - mine:    { then: lifetime }
  - lifetime:
      fork:
        compete: true
        branches:
          - process:                    # the stage dispatcher (D-80 as amended)
              do:
                - dispatch:
                    switch:
                      - approval:    { when: '${ $context.nextStage == "approval" }',    then: approval }
                      - fulfillment: { when: '${ $context.nextStage == "fulfillment" }', then: fulfillment }
                      - failure:     { when: '${ $context.nextStage == "failure" }',     then: failure }
                      - unwind:      { when: '${ $context.nextStage == "unwind" }',      then: unwind }
                      - cancel:      { when: '${ $context.nextStage == "cancel" }',      then: cancel }
                      - ceiling:     { when: '${ $context.nextStage == "ceiling" }',     then: ceiling }
                      - hold:        { when: '${ $context.nextStage == "hold" }',        then: hold }
                      - resume:      { when: '${ $context.nextStage == "resume" }',      then: resume }
                      - lifecycle:   { when: '${ $context.nextStage == "lifecycle" }',   then: lifecycle }
                      - unknown:     { then: unknownStage }
                - unknownStage:         # a defect of the version, never a business outcome: fault the invocation
                    raise:
                      error:
                        type: https://serverlessworkflow.io/spec/1.0.0/errors/runtime
                        status: 500
                        title: unknown-stage
                        detail: nextStage names no stage task of this version   # a literal: the 1.0.0 schema types error.detail as a plain string; the faulted task's position names the dispatcher
                - approval:    { do: [ ‹approval stage, below› ],       then: dispatch }
                - fulfillment: { do: [ ‹fragment (b)› ],               then: dispatch }
                - failure:     { do: [ ‹failure stage, fragment (c)› ], then: dispatch }
                - unwind:      { do: [ ‹unwind stage, fragment (c)› ],  then: dispatch }
                - cancel:      { do: [ ‹cancel stage, fragment (d)› ],  then: dispatch }
                - ceiling:     { do: [ ‹ceiling stage, fragment (d)› ], then: dispatch }
                - hold:        { do: [ ‹hold stage, fragment (e)› ],    then: dispatch }
                - resume:      { do: [ ‹resume stage, fragment (e)› ],  then: dispatch }
                - lifecycle:   { do: [ ‹fragment (f)› ],               then: dispatch }
          - lifetimeCeiling:            # outside every stage: a hold never pauses it (08 §4.7 item 2)
              do:
                - waitCeiling: { wait: P90D }
                - arm: { set: { arm: lifetime } }
          - overdueMonitor: { do: [ ‹overdue monitor, fragment (b)› ] }   # never completes
  - afterLifetime:                      # only lifetimeCeiling completes: every process path ends the workflow with `end`
      switch:
        - unwinding: { when: '${ .arm == "lifetime" and $context.unwind != null }', then: lifetime }   # set on every entry to the unwind and never cleared, so it covers a cancel or a task taken from inside it: no compensating → parked edge (01 §3.7); the unwind continues under a fresh ceiling (D-121)
        - verdictPark: { when: '${ .arm == "lifetime" and $context.stageLoop == "parkLoop" }', then: lifetime }   # the verdict park, or a hold, resume, lifecycle or cancel stage taken from it: already parked and escalated by its own clock; no parked → parked edge, and the park ends only by its three routes (03 §4.1, D-121)
        - ceilingParked: { when: '${ .arm == "lifetime" and $context.returnStage == "ceiling" }', then: lifetime }   # a lifecycle or cancel stage taken from the ceiling wait: the instance is already parked at a ceiling whose task is open and has its own SLA tick; no parked → parked edge, and ceilingEntry must not overwrite that ceiling's saved return state (D-163)
        - ceiling:   { when: '${ .arm == "lifetime" }', then: ceilingEntry }
        - other:     { then: end }
  - ceilingEntry:                       # records where the process was, then re-enters the fork at the ceiling stage of fragment (d)
      set:
        ceilingReturnStage: '${ if $context.nextStage == "ceiling" then $context.ceilingReturnStage else $context.nextStage end }'
        ceilingReturnLoop:  '${ if $context.nextStage == "ceiling" then $context.ceilingReturnLoop else $context.stageLoop end }'
        ceilingReturnBack:  '${ if $context.nextStage == "ceiling" then $context.ceilingReturnBack else $context.returnStage end }'   # the interrupted stage's own return: the ceiling stage's leave overwrites returnStage, and backToProcess restores it (D-161)
        nextStage: ceiling
      then: lifetime
```

The approval stage, the `do` list of `process.approval`:

```yaml
- enter:                                # re-entry at the checkpoint the stage recorded (R-A)
    switch:
      - resumedDue: { when: '${ $context.stageLoop == "gateLoop" and $context.resumeDue == true }', then: escalateGate }   # apply-resume's due is the first answer (08 §4.7 item 1)
      - parkLoop:   { when: '${ $context.stageLoop == "parkLoop" }',       then: park }        # the checkpoint's call re-issued under its unchanged key, then the loop
      - gateLoop:   { when: '${ $context.stageLoop == "gateLoop" }',       then: escalateGate }   # every return into the gate loop fires first, so an arm taken between two fires adds only its own calls (D-148)
      - outageArm:  { when: '${ $context.stageLoop == "outageArm" }',      then: outageArm }
      - decision:   { when: '${ $context.stageLoop == "recordDecision" }', then: recordDecision }   # a consumed decision is re-issued under its unchanged key, never dropped (D-147)
      - reflect:    { when: '${ $context.stageLoop == "reflectVerdict" }', then: reflectVerdict }
      - fresh:      { then: obtainVerdict }
- obtainVerdict:                        # protected (03)
    timeout: step
    try:
      - call: { step: obtain-verdict }  # ‹abridged call›; body: ref + round: $context.verdictRound; output: verdict ∈ required | not-required | unobtainable; parkRef, parkReason on unobtainable; nextRound
    catch: *transient
    export: { as: '${ $context + { verdict: .verdict, parkRef: .parkRef, parkReason: .parkReason, reflectStage: "requirement", reflectRound: 0, verdictRound: .nextRound } }' }
- onVerdict:                            # 03 §4.5 item 2: unobtainable routes only to the park arm
    switch:
      - unobtainable: { when: '${ $context.verdict == "unobtainable" }', then: enterParkLoop }
      - obtained:     { then: reflectVerdict }
- enterParkLoop: { set: { stageLoop: parkLoop, parkRound: 0, holdPauses: false } }   # recorded before the park, so a ceiling that interrupts the call finds the park loop (afterLifetime); holdPauses false: a hold here is recorded, never waited on
- park:                                 # fail-closed park, ADR-0007 as amended; the park loop's hold and resume arms only record (03 §4.5 item 6)
    timeout: step
    try:
      - call: { step: park }            # body: ref + parkReason: $context.parkReason, subjectRef: $context.parkRef; keyed by the parkRef, so the re-issue on re-entry is a replay
    catch: *transient
- parkLoop:
    fork:
      compete: true
      branches:
        - tick:      { do: [ { waitTtlMargin: { wait: PT5M } }, { arm: { set: { arm: tick, heldTicks: '${ if $context.suspensionRef != null then ($context.heldTicks // 0) + 1 else 0 end }' } } } ] }   # heldTicks: ticks since a recorded suspension was last polled (D-133)
        - hold:      { do: [ ‹hold arm of (e)› ] }       # records the suspension on the parked instance (08); the park clock is not paused
        - resume:    { do: [ ‹resume arm of (e)› ] }
        - lifecycle: { do: [ ‹lifecycle arm of (f)› ] }
        - cancel:    { do: [ ‹cancel arm of (d)› ] }
- afterParkLoop:
    switch:
      - heldPoll: { when: '${ .arm == "tick" and $context.heldTicks >= 3 }', then: pollHeld }   # every third PT5M tick while a suspension is recorded (D-133)
      - tick:  { when: '${ .arm == "tick" }', then: retryVerdict }
      - other: { then: leave }                                   # hold | resume | lifecycle | cancel; hold and resume come back to parkLoop
- retryVerdict:
    timeout: step
    try:
      - call: { step: obtain-verdict }  # body: ref + round: $context.verdictRound; every answer returns the next round
    catch: *transient
    export: { as: '${ $context + { verdict: .verdict, verdictRound: .nextRound } }' }
- afterRetry:
    switch:
      - obtained: { when: '${ $context.verdict != "unobtainable" }', then: unparkForVerdict }   # unpark only after required | not-required
      - still:    { then: checkParkEscalation }
- checkParkEscalation:                  # composable (03): the re-check of the park's escalation_due_at; due is false once the park has escalated
    timeout: step
    try:
      - call: { step: arm-park-escalation }   # body: ref + parkRef, round: $context.parkRound; output: due, nextRound
    catch: *transient
    export: { as: '${ $context + { parkEscalationDue: .due, parkRound: .nextRound } }' }
- onParkEscalation:
    switch:
      - due:    { when: '${ $context.parkEscalationDue }', then: escalatePark }
      - notDue: { then: parkLoop }      # a settled success of its round; the next tick calls the next round
- escalatePark:                         # recorded once; the park clock is never re-armed (03 §4.5 item 6)
    timeout: step
    try:
      - call: { step: raise-overdue-escalation }   # body: ref + escalationKind: park, subjectRef: $context.parkRef
    catch: *transient
    then: parkLoop
- unparkForVerdict:
    timeout: step
    try:
      - call: { step: unpark }          # body: ref + subjectRef: $context.parkRef
    catch: *transient
- leftPark: { set: { stageLoop: reflectVerdict } }   # out of the park loop: a ceiling from here parks (afterLifetime)
- onLeftPark:                           # a suspension recorded while parked is polled once more before the gates, so it is not carried into gateLoop, which does not poll (D-133)
    switch:
      - heldPoll: { when: '${ $context.suspensionRef != null }', then: pollHeld }
      - reflect:  { then: reflectVerdict }
- reflectVerdict:                       # protected (03): requirement stage submitted → pending_approval | approved; gate-outcome stage pending_approval → approved | rejected (R1)
    try:
      - reflect:
          timeout: step
          try:
            - call: { step: reflect-verdict }   # body: ref + stage: $context.reflectStage, round: $context.reflectRound, attemptKey: $context.attemptKey; output: reflected, nextRound
          catch: *transient
    catch:                              # a permanent refusal: approval-reflection-refused needs a human (03 §4.5 item 3)
      errors: { with: { type: https://serverlessworkflow.io/spec/1.0.0/errors/communication, status: 400 } }
      do: [ { refused: { set: { reflected: refused } } } ]
    export: { as: '${ $context + { reflected: .reflected, reflectRound: (.nextRound // $context.reflectRound), reflectAttempt: (($context.reflectRound | tostring) + (if $context.attemptKey then ":" + ($context.attemptKey | tostring) else "" end)), attemptKey: null } }' }   # reflectAttempt: the call's key tail {round}[:{attempt}], the task's sourceAttempt; reflected ∈ pending_approval | approved | rejected | held | moved | refused; a retry's attempt is spent once the call settles
- afterReflect:
    switch:
      - approved: { when: '${ $context.reflected == "approved" }', then: toFulfillment }
      - rejected: { when: '${ $context.reflected == "rejected" }', then: terminateRejected }
      - refused:  { when: '${ $context.reflected == "refused" }',  then: reflectionTask }
      - held:     { when: '${ $context.reflected == "held" }',     then: enterHeldReflect }   # Lifecycle not-admissible on an on_hold order (03 §4.4): wait for the resume, then the next round
      - moved:    { when: '${ $context.reflected == "moved" }',    then: enterHeldReflect }   # Lifecycle version-conflict, or not-admissible on a terminal order (03 §4.4): the lifecycle arm consumes OrderAmended or the terminal event
      - pending:  { then: firstPosition }
- enterHeldReflect: { set: { stageLoop: reflectVerdict, holdPauses: true } }   # re-entry after the resume goes straight to reflectVerdict
- awaitHeldReflect:
    fork:
      compete: true
      branches:
        - tick:      { do: [ { waitHeldReflect: { wait: PT5M } }, { arm: { set: { arm: tick, heldTicks: '${ if $context.suspensionRef != null then ($context.heldTicks // 0) + 1 else 0 end }' } } } ] }   # covers a resume consumed before this wait was armed
        - hold:      { do: [ ‹hold arm of (e)› ] }
        - resume:    { do: [ ‹resume arm of (e)› ] }
        - lifecycle: { do: [ ‹lifecycle arm of (f)› ] }
        - cancel:    { do: [ ‹cancel arm of (d)› ] }
- afterHeldReflect:
    switch:
      - heldPoll: { when: '${ .arm == "tick" and $context.heldTicks >= 3 }', then: pollHeld }   # D-133
      - tick:  { when: '${ .arm == "tick" }', then: reflectVerdict }
      - other: { then: leave }          # hold | resume | lifecycle | cancel
- toFulfillment: { set: { nextStage: fulfillment, stageLoop: null }, then: exit }   # Lifecycle emits OrderApproved; fragment (b) follows in the same invocation
- reflectionTask:                       # fragment (c): an order-scope task whose retry re-enters reflectVerdict
    set: { failureScope: order, failureSubjects: '${ [ { subjectRef: $context.correlationId, reason: "approval-reflection-refused", cause: "permanent-refusal" } ] }', sourceStep: reflect-verdict, sourceAttempt: '${ $context.reflectAttempt }', forceTask: true, taskReturnStage: approval, taskReturnLoop: reflectVerdict, nextStage: failure, stageLoop: null }
    then: exit
- terminateRejected:                    # protected (01); terminationKind: rejected
    timeout: step
    try:
      - call: { step: terminate-instance }
    catch: *transient
    then: end
- firstPosition: { set: { position: 0, reflectStage: gate-outcome, reflectRound: 0 } }   # 03 §4.5 item 7: 0 first, thereafter record-decision's nextPosition only
- openGates:                            # composable (03): opens every gate at the current sequence position
    timeout: step
    try:
      - call: { step: open-gates }      # body: ref + position: $context.position; output: gateRefs[], position, escalationRound
    catch: *transient
    export: { as: '${ $context + { gateRefs: .gateRefs, escalationRound: .escalationRound, probeRound: 0, resumeDue: false } }' }
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
                        with: { type: ‹approval decision event type, 03 §3.3› }
                        correlate:
                          orderId:      { from: '${ .data.orderId }',      expect: '${ $context.orderId }' }
                          orderVersion: { from: '${ .data.orderVersion }', expect: '${ $context.orderVersion }' }
                    read: envelope
                  output: { as: '${ .[0] | { gateRef: .data.gateId, decisionEventId: .data.decisionEventId, outcome: .data.outcome } }' }
              - arm: { set: { arm: decision, gateRef: '${ .gateRef }', decisionEventId: '${ .decisionEventId }', outcome: '${ .outcome }' } }
        - probe:      { do: [ { waitProbe: { wait: PT30S } }, { arm: { set: { arm: probe } } } ] }   # 03 §4.5 items 4, 5: the escalation re-check, then the probe, ≤ 30 s; one tick, so no longer wait competes with it and loses on every probe (D-123)
        - hold:       { do: [ ‹hold arm of (e)› ] }
        - resume:     { do: [ ‹resume arm of (e)› ] }       # early resume, 08 §4.7 item 3
        - lifecycle:  { do: [ ‹lifecycle arm of (f)› ] }
        - cancel:     { do: [ ‹cancel arm of (d)› ] }
- afterGateLoop:
    switch:
      - decision: { when: '${ .arm == "decision" }', then: enterRecordDecision }
      - probe:    { when: '${ .arm == "probe" }',    then: escalateGate }
      - other:    { then: leave }       # hold | resume | lifecycle | cancel
- enterRecordDecision: { set: { stageLoop: recordDecision } }   # the decision arm consumed its event: a checkpoint before the call, so a ceiling that interrupts it re-issues it (D-147)
- recordDecision:                       # protected (03)
    timeout: gate
    try:
      - call: { step: record-decision } # body: ref + gateRef, decisionEventId, outcome ∈ approved | rejected
    catch: &gateTransient               # *transient's status set under the gate retry policy, whose worst case fits the gate timeout (D-162)
      errors: { with: { type: https://serverlessworkflow.io/spec/1.0.0/errors/communication } }
      when: '${ $error.status as $s | any((429, 503, 504, 409); . == $s) }'
      retry: gate
    export: { as: '${ $context + { gateState: .gateState, position: .nextPosition } }' }
- afterDecision:
    switch:
      - decided:      { when: '${ $context.gateState == "approved" or $context.gateState == "rejected" }', then: reflectVerdict }   # reflectStage: gate-outcome
      - nextPosition: { when: '${ $context.gateState == "next-position" }', then: openGates }
      - pending:      { then: reenterGateLoop }
- reenterGateLoop: { set: { stageLoop: gateLoop }, then: escalateGate }   # a pending decision fires before the next tick (D-148)
- escalateGate:                         # composable (03): the escalation re-check; fires, and enqueues OrderApprovalEscalated, only when due
    timeout: gate
    try:
      - call: { step: escalate-gate }   # body: ref + position, mode: fire, round: $context.escalationRound; output: due, escalationRound
    catch: *gateTransient
    export: { as: '${ $context + { escalationRound: .escalationRound, resumeDue: false } }' }
    then: probeGate                     # due routes nothing: true has escalated inside the operation, false is a settled success of its round; the next tick carries the next escalationRound
- probeGate:
    timeout: gate
    try:
      - call: { step: escalate-gate }   # body: ref + position, mode: probe, round: $context.probeRound; output: serviceState, due, probeRound
    catch: *gateTransient
    export: { as: '${ $context + { serviceState: .serviceState, probeRound: .probeRound } }' }
- afterProbe:
    switch:
      - outage:    { when: '${ $context.serviceState == "outage" }', then: enterOutageArm }
      - available: { then: gateLoop }
- enterOutageArm: { set: { stageLoop: outageArm, holdPauses: true, outageEscalated: false } }
- outageArm:                            # 03 §4.5 item 5: probe loop until available, the outage threshold, hold and cancel
    fork:
      compete: true
      branches:
        - probe:     { do: [ { waitProbe: { wait: PT30S } }, { arm: { set: { arm: probe } } } ] }   # also the outage-threshold re-check
        - hold:      { do: [ ‹hold arm of (e)› ] }
        - resume:    { do: [ ‹resume arm of (e)› ] }
        - lifecycle: { do: [ ‹lifecycle arm of (f)› ] }
        - cancel:    { do: [ ‹cancel arm of (d)› ] }
- afterOutageArm:
    switch:
      - probe: { when: '${ .arm == "probe" }', then: probeOutage }
      - other: { then: leave }
- probeOutage:                          # under the gate timeout and policy: its available answer is followed by a fire, so it is one of the calls between two fires (D-162)
    timeout: gate
    try:
      - call: { step: escalate-gate }   # body: ref + position, mode: probe, round: $context.probeRound; due: the outage-threshold deadline on outage
    catch: *gateTransient
    export: { as: '${ $context + { serviceState: .serviceState, outageDue: (.due // false), probeRound: .probeRound } }' }
- afterProbeOutage:
    switch:
      - available: { when: '${ $context.serviceState == "available" }', then: outageOver }
      - threshold: { when: '${ $context.outageDue and ($context.outageEscalated | not) }', then: escalateOutage }
      - still:     { then: outageArm }
- outageOver: { set: { stageLoop: gateLoop, holdPauses: true }, then: escalateGate }   # the return from the outage arm fires before it waits, like every other return into the gate loop (D-162)
- escalateOutage:                       # recorded once per outage
    timeout: step
    try:
      - call: { step: raise-overdue-escalation }   # body: ref + escalationKind: approval-outage, subjectRef: $context.position
    catch: *transient
    export: { as: '${ $context + { outageEscalated: true } }' }
    then: outageArm
- pollHeld:                             # protected (08): the resume wait's poll, from the park loop, the held-reflection wait or the way out of the park (D-133); no intent exists before begin-fulfillment, so failedTaskRefs is empty here
    timeout: step
    try:
      - call: { step: apply-resume }    # body: ref + trigger: poll, suspensionRef, round: $context.resumePollRound; output: resumeOutcome ∈ resumed-by-read | still-held, due, failedTaskRefs[], nextRound
    catch: *transient
    export: { as: '${ $context + { resumeOutcome: .resumeOutcome, resumePollRound: .nextRound, heldTicks: 0, suspensionRef: (if .resumeOutcome == "resumed-by-read" then null else $context.suspensionRef end) } }' }
- afterHeldPoll:                        # either answer continues with the tick's own work; decided by stageLoop alone
    switch:
      - park:    { when: '${ $context.stageLoop == "parkLoop" }', then: retryVerdict }
      - reflect: { then: reflectVerdict }   # awaitHeldReflect or leftPark: stageLoop reflectVerdict
- leave: { set: { nextStage: '${ .arm }', returnStage: approval }, then: exit }
```

**Description**: The fragment reproduces [`02 §3.6`](./02-triggers-and-start.md#36-interactions--sequences)
*Start on trigger* and *Duplicate absorption* and [`03 §3.6`](./03-approval-execution.md#36-interactions--sequences)
*Verdict retrieval and reflection*, *Multi-party gate open* and *Decision reflection*, with the
timer and the pause moved into the definition. **Start.** The invocation starts on either
platform event trigger of §3.3 (`OrderSubmitted` for a first version, `OrderAmended` for an
amended one — Lifecycle publishes no `OrderSubmitted` after an amendment,
[Lifecycle `04 §4.3`](../../../orders-lifecycle/docs/design/04-versioning.md#43-re-approval-is-a-two-step-seam-interaction-normative));
the document declares no `schedule`, so the trigger bindings are the one start mechanism. The
`input.from` expression reads the trigger's event envelope as the invocation input; the shape a
trigger passes is not stated by the platform and is part of the trigger ask
([`../UPSTREAM_REQS.md`](../UPSTREAM_REQS.md) §2.9 *Event triggers over event-broker GTS events*).
Only `admission = start` reaches `start-instance`, and every other admission ends the invocation
with no further call. A `prior-instance-active` refusal (409) is retried on the constant
5-minute `supersession` policy for up to 24 h while the prior version's instance unwinds; the
inner `*transientNo409` catch handles the rest of the transient set. Because the plugin may not
surface `error_code` on `$error` (Q-11 (ii)), `still-processing` and `idempotency-lease-expired`
also ride the supersession cadence on this one call; the key stays `open` and the answer is the
same. **Approval.** The escalation window is Orders' stored deadline, re-checked by
`escalate-gate` `mode: fire` on every `PT30S` `waitProbe` tick, before the probe; `due: false`
escalates nothing and is a settled success of its round, and the next tick calls the next round.
The re-check rides the probe tick because two waits of different lengths re-armed on every pass
of one competing fork let the shorter always win, so a separate `PT5M` escalation wait never
fired (decision D-123). The lateness bound counts every call that can run between two fires:
the rest of the fire whose read the deadline just missed, the probe, the tick — or, when a
decision wins the tick, `record-decision` — and the next fire. Those calls carry the 60-second
`gate` timeout, a pending decision goes straight to `escalateGate` (`reenterGateLoop`), the
outage arm's `available` answer goes to `escalateGate` through `outageOver`, and every re-entry of
the stage at `gateLoop` fires before it waits, so the worst case with every call's retries running
to its timeout is 30 s + 4 × 60 s = 4 min 30 s, inside the ± 5 min of
`nfr-owf-escalation-timer`. The timeout is the bound only if it, not the retry limit, is what a
spent call can reach, and the `transient` curve does not fit it: 5 attempts of up to the 10 s
`deadline_ms` plus four gaps of 1–8 s backoff and up to 30 s of jitter each reach about 185 s, so
the 60 s timeout would fire after two or three attempts and fault the invocation with a 408 that
no `catch` takes. These four calls therefore retry under their own `gate` policy — 4 attempts,
constant 2 s delay, jitter 0–3 s — whose worst case, 4 × 10 s + 3 × (2 s + 3 s) = 55 s, is under
the timeout, so the retry limit ends a spent call first and the 4 × 60 s sum still bounds every
call (decision D-162). The price is a shorter absorption window: an Orders step-surface or
approval-service blip that outlasts four attempts, about 6–15 s of spacing when the failures are
fast, faults the invocation as any spent step call does (§4.6) and is re-driven, where the
`transient` curve would have absorbed about a minute. An arm that leaves the stage and returns — a denied cancel, an
absorbed lifecycle event, an early resume — adds only the calls of the stage it visited, each
normally inside its 5–10 s `deadline_ms`; only with those calls' retries running to the 3-minute
`step` timeout can the bound be exceeded, the residual of a degraded Orders, and a hold pauses the
window itself (decision D-148). A decision, a hold or a cancel winning the race
cancels only the tick; the pause itself is in Orders' record, where
`apply-hold` pauses the gate window through slice 03's gate-window port and `apply-resume`
re-bases it, so the definition carries no remainder. `open-gates` receives `position = 0` first
and `record-decision`'s `nextPosition` thereafter; `escalate-gate` receives the `escalationRound`
or `probeRound` it last returned. The **approval-service outage** is observed by the `probe`
branch (`escalate-gate` `mode: probe`); an `outage` answer enters `outageArm`, whose probe also
re-checks the outage threshold (`due` on `outage`) and leads to one `raise-overdue-escalation`
with `escalationKind: approval-outage`, and an `available` answer re-enters `gateLoop`. **Park.**
The park loop carries one `PT5M` tick, the lifecycle and the cancel arms, and a hold and a resume
arm that only record (`holdPauses` is false), so a hold during a park is known to Orders when the
reflection after the `unpark` runs, and the park clock is never paused; each
tick retries `obtain-verdict` under the next round and, while the verdict is still unobtainable, re-checks the park's
`escalation_due_at` through `arm-park-escalation`, whose `due` is `false` once the park escalation
is recorded, so the park clock is never re-armed. A repeat `obtain-verdict` for a version whose
verdict is already reflected returns the cached answer inside the operation and never
re-reflects, as `03` requires. A permanent refusal of `reflect-verdict`
(`approval-reflection-refused`) goes to an order-scope manual task (fragment (c)), whose retry
re-enters this stage at `reflectVerdict` carrying the `attemptKey` `retry-step` minted, so the
retry is a new key rather than a replay of the stored refusal. A `held` reflection — Lifecycle
refused `not-admissible` and the order is `on_hold` — waits in `awaitHeldReflect` for the resume
(or the `PT5M` tick) and reflects again under the next round ([`01 §3.3` *Rounds and attempts*](./01-foundation.md#rounds-and-attempts-the-one-rule-for-re-invokable-operations)). A `moved` reflection — Lifecycle
refused `version-conflict`, or `not-admissible` on an order it holds terminal — waits in the same
fork, whose lifecycle arm consumes the `OrderAmended` or terminal event that moved the order: a
stale result is not a failure of the reflection it reports
([Lifecycle `04 §4.4`](../../../orders-lifecycle/docs/design/04-versioning.md#44-stale-results-normative),
decision D-112). The approved path ends at `reflect-verdict`; Lifecycle
emits `OrderApproved`, and the fulfillment stage is entered through the dispatcher in the same
invocation rather than from a second trigger. **Lifetime.** The ceiling is the top-level
competing arm, a literal `P90D`. When it fires, `ceilingEntry` records the stage and checkpoint
the process was in and re-enters the `lifetime` fork at the ceiling stage of fragment (d), which
raises the escalation and parks; the re-entered fork arms a fresh ceiling, so an instance the
operator unparks continues under a new 90-day bound, and each ceiling is its own round
(`ceilingRound`): its escalation, its task and its park subject `ceiling:{round}` are new, so a
second ceiling is never a replay of the first (decision D-121). `afterLifetime` decides from the
definition's own state, never from `nextStage` alone: a ceiling that fires anywhere inside an
unwind — `unwind` is set on every entry to it and never cleared, so a cancel or a task taken from
it is still inside — does not park (no `compensating → parked` edge, `01 §3.7`), and a ceiling
that fires in the verdict park loop, or in a stage taken from it, does not park again (no
`parked → parked` edge) and adds no route out of that park (`03 §4.1`); in both cases the process
continues under the fresh ceiling. `enterParkLoop` is recorded before `park` so that a ceiling
interrupting the call still finds the park loop, and `leftPark` after `unpark` so that one after
it parks.

#### (b) Fulfillment: eligibility, plan, two waves and the barrier

**ID**: `cpt-cf-bss-orders-workflow-seq-def-fulfillment`

**Use cases**: `cpt-cf-bss-orders-workflow-fr-owf-payment-auth`, `cpt-cf-bss-orders-workflow-fr-owf-fulfillment-plan`, `cpt-cf-bss-orders-workflow-fr-owf-provisioning-intent`, `cpt-cf-bss-orders-workflow-fr-owf-line-progress`, `cpt-cf-bss-orders-workflow-fr-owf-overdue-escalation`

**Actors**: `cpt-cf-bss-orders-workflow-actor-owf-orders-lifecycle`, `cpt-cf-bss-orders-workflow-actor-owf-payments`, `cpt-cf-bss-orders-workflow-actor-owf-subscriptions`

The fulfillment stage, the `do` list of `process.fulfillment`:

```yaml
- enter:
    switch:
      - eligibilityWait: { when: '${ $context.stageLoop == "awaitEligibilityChange" }', then: awaitEligibilityChange }
      - plan:            { when: '${ $context.stageLoop == "freezePlan" }',             then: freezePlan }
      - wave1:           { when: '${ $context.stageLoop == "wave1" }',                  then: wave1 }
      - expected:        { when: '${ $context.stageLoop == "awaitExpected" }',          then: evaluate }
      - barrier:         { when: '${ $context.stageLoop == "barrierLoop" }',            then: evaluate }
      - heldSpawn:       { when: '${ $context.stageLoop == "heldSpawn" }',              then: preActivation }     # after a resume (or a recorded hold): Lifecycle re-runs the re-check after a hold, then the spawn signal under its next round
      - heldReport:      { when: '${ $context.stageLoop == "heldReport" }',             then: reportCompleted }   # the completion report under its next round
      - planFailFast:    { when: '${ $context.stageLoop == "planFailFast" }',           then: planFailFast }   # from the failure stage: an exhausted plan task passes begin-fulfillment before its unwind (D-109)
      - fresh:           { then: initEligibility }
- initEligibility: { set: { eligibilityTrigger: initial, requestRef: null, evaluationSeq: 0, barrierSeq: 0, recheckSeq: 0, wave1Round: 0, wave2Round: 0, rebuildRound: 0, sweepRound: 0, spawnRound: 0, reportRound: 0, planAttempt: 0, wave1Failed: [], wave2Failed: [], spawned: false, planFailed: false, wave1AttemptKey: null, wave2AttemptKey: null } }
- eligibility:                          # protected (04)
    timeout: step
    try:
      - call: { step: evaluate-payment-auth-eligibility }   # body: ref + trigger: $context.eligibilityTrigger, requestRef: $context.requestRef, evaluationSeq: $context.evaluationSeq
    catch: *transient
    export: { as: '${ $context + { eligibility: .eligibility, eligibilitySeq: (if .eligibility == "eligible" then $context.evaluationSeq else $context.eligibilitySeq end), evaluationSeq: .nextEvaluationSeq } }' }
- onEligibility:
    switch:
      - planFailed: { when: '${ $context.eligibility == "eligible" and $context.planFailed }', then: planFailFast }   # a failed plan waiting to pass begin-fulfillment (D-109)
      - eligible: { when: '${ $context.eligibility == "eligible" }', then: enterPlan }
      - waiting:  { then: enterEligibilityWait }   # pending: a settled success that selects the wait; the order stays approved
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
                    read: envelope
                  output: { as: '${ .[0] | { eventId: .id } }' }
              - arm: { set: { arm: acceptance, lifecycleEventId: '${ .eventId }', triggerKind: OrderAcceptanceRecorded, preAdmitted: false } }
        - reauthorize:
            do:
              - listenReauth:
                  listen:
                    to:
                      one:
                        with: { type: gts.cf.core.events.event.v1~cf.bss.orders_workflow.signal.v1~cf.bss.orders_workflow.reauthorize_requested.v1~ }
                        correlate:
                          orderId:      { from: '${ .data.orderId }',      expect: '${ $context.orderId }' }
                          orderVersion: { from: '${ .data.orderVersion }', expect: '${ $context.orderVersion }' }
                    read: envelope
                  output: { as: '${ .[0] | { requestRef: .data.requestRef } }' }
              - arm: { set: { arm: reevaluate, eligibilityTrigger: reauthorize-requested, requestRef: '${ .requestRef }' } }
        - poll:      { do: [ { waitEligibility: { wait: PT5M } }, { arm: { set: { arm: reevaluate, eligibilityTrigger: poll, requestRef: null, heldTicks: '${ if $context.suspensionRef != null then ($context.heldTicks // 0) + 1 else 0 end }' } } } ] }   # 04 §4.8 item 5: covers an event delivered before the listen was armed
        - hold:      { do: [ ‹hold arm of (e)› ] }
        - resume:    { do: [ ‹resume arm of (e)› ] }
        - lifecycle: { do: [ ‹lifecycle arm of (f)› ] }
        - cancel:    { do: [ ‹cancel arm of (d)› ] }
- afterEligibilityChange:
    switch:
      - acceptance: { when: '${ .arm == "acceptance" }', then: admitAcceptance }
      - heldPoll:   { when: '${ .arm == "reevaluate" and $context.eligibilityTrigger == "poll" and $context.heldTicks >= 3 }', then: pollHeld }   # D-133
      - again:      { when: '${ .arm == "reevaluate" }', then: eligibility }
      - other:      { then: leave }
- admitAcceptance:                      # protected (02): admission before consumption (02 §4.7 item 1)
    timeout: step
    try:
      - call: { step: admit-trigger }   # body: ref + triggerEventId: $context.lifecycleEventId, triggerKind, role: listen
    catch: *transient
    export: { as: '${ $context + { admission: .admission, supersededByOrderVersion: (if .admission == "supersede" then .currentOrderVersion else null end) } }' }   # the version Lifecycle's read returned, the only source of the supersession audit (02 §4.3, D-155)
- onAcceptanceAdmission:
    switch:
      - advance:  { when: '${ $context.admission == "advance" }', then: acceptanceRecorded }
      - onward:   { when: '${ $context.admission == "supersede" or $context.admission == "terminate" }', then: toLifecycle }
      - back:     { then: awaitEligibilityChange }
- toLifecycle: { set: { nextStage: lifecycle, preAdmitted: true, returnStage: fulfillment }, then: exit }   # fragment (f) routes the admitted answer
- acceptanceRecorded: { set: { eligibilityTrigger: acceptance-recorded, requestRef: null }, then: eligibility }
- enterPlan: { set: { stageLoop: freezePlan } }
- freezePlan:                           # protected (04)
    timeout: step
    try:
      - call: { step: construct-and-freeze-plan }   # body: ref + attempt: $context.planAttempt; output: planRef, lineRefs[], expectedFulfillmentAt, policy, planState, reason
    catch: *transient
    export: { as: '${ $context + { planRef: .planRef, lineRefs: .lineRefs, wave1LineRefs: .lineRefs, policy: .policy, planState: .planState, planReason: .reason } }' }
- onPlan:                               # 04 §4.8 item 3 and §4.3
    switch:
      - frozen:           { when: '${ $context.planState == "frozen" }', then: beginFulfillment }
      - topology:         { when: '${ $context.planState == "topology-unavailable" }', then: planTask }   # a plan task under EITHER policy
      - invalidRemediate: { when: '${ $context.policy == "remediate" }', then: planTask }
      - invalidFailFast:  { then: planFailFast }
- planTask:                             # fragment (c)
    set: { failureScope: plan, failureSubjects: '${ [ { subjectRef: $context.planRef, reason: $context.planReason, cause: "plan-not-frozen" } ] }', sourceStep: construct-and-freeze-plan, sourceAttempt: '${ $context.planAttempt | tostring }', forceTask: true, nextStage: failure, stageLoop: null }
    then: exit
- planFailFast:                         # no Workflow transition leaves approved: begin-fulfillment is passed before the unwind (04 §4.3); also the failure stage's route for an exhausted plan task (D-109)
    timeout: step
    try:
      - call: { step: begin-fulfillment }   # body: ref + planRef, eligibilitySeq; admitted for a plan that did not freeze (04 §3.6 inst-bf-guard-frozen)
    catch: *transient
    export: { as: '${ $context + { beginResult: .result } }' }
- onPlanFailFast:
    switch:
      - begun:   { when: '${ $context.beginResult == "in-fulfillment" }', then: planFailFastUnwind }
      - waiting: { then: enterPlanFailedWait }   # withheld | held | version-conflict: the order is not in fulfillment, so no failure can be acknowledged yet
- enterPlanFailedWait: { set: { planFailed: true }, then: enterEligibilityWait }   # the next eligible round calls begin-fulfillment under a new eligibilitySeq; the lifecycle arm ends the wait on an amendment or expiry
- planFailFastUnwind: { set: { unwind: failure, nextStage: unwind, stageLoop: null }, then: exit }   # fragment (c): nothing to void; fulfillment_failed with dependency-graph-invalid (06 §4.8)
- beginFulfillment:                     # protected (04): the R1 seam call approved → in_fulfillment; enqueues OrderFulfillmentStarted
    timeout: step
    try:
      - call: { step: begin-fulfillment }   # body: ref + planRef, eligibilitySeq: $context.eligibilitySeq; output: result ∈ in-fulfillment | withheld | held | version-conflict, withheldCause
    catch: *transient                   # 04 §3.6: every 409 of this operation is still-processing, re-issued under the same key; routing answers are settled successes
    export: { as: '${ $context + { beginResult: .result } }' }
- onBegin:                              # 04 §4.8 item 2
    switch:
      - started: { when: '${ $context.beginResult == "in-fulfillment" }', then: enterWave1 }
      - waiting: { then: enterEligibilityWait }   # withheld | held | version-conflict (the amendment arm is in the wait, 02 §4.7 item 8); the next eligible round carries a new eligibilitySeq
- enterWave1: { set: { stageLoop: wave1, holdPauses: false } }   # the wave waits record a hold and do not wait on the resume (D-142)
- wave1:                                # protected (05): ONE call per wave carrying lineRefs[]; per-order parallelism and admission inside (05 §4.3)
    try:
      - dispatchWave1:
          timeout: wave1
          try:
            - call: { step: dispatch-wave1-create }   # body: ref + planRef, lineRefs: $context.wave1LineRefs, dispatchRound: $context.wave1Round, attemptKey: $context.wave1AttemptKey (this wave's own attempt, D-119); output: accepted[], failed[], deferred[], due, nextDispatchRound
          catch: *transientNo409
    catch:                              # 409: still-processing | idempotency-lease-expired (Q-11 (ii)), read before re-issuing (05 §4.5 item 3); otherwise the budget or the timeout is spent
      as: waveError
      when: '${ $waveError.status as $s | any((409, 408, 429, 503, 504); . == $s) }'
      do: [ { classify: { set: { waveOutcome: '${ if $waveError.status == 409 then "reread" else "exhausted" end }', waveCause: '${ if $waveError.status == 408 then "step-deadline-exceeded" else "retry-budget-exhausted" end }' } } } ]
    export: { as: '${ $context + { waveOutcome: (.waveOutcome // "answered"), waveCause: (.waveCause // null), wave1Failed: ($context.wave1Failed + (.failed // [])), wave1Deferred: (.deferred // []), wave1DeferReason: (.deferReason // null), wave1Round: (.nextDispatchRound // $context.wave1Round), wave1Attempt: (($context.wave1Round | tostring) + (if $context.wave1AttemptKey then ":" + ($context.wave1AttemptKey | tostring) else "" end)), wave1AttemptKey: (if (.waveOutcome // "answered") == "answered" then null else $context.wave1AttemptKey end) } }' }   # wave1Attempt: the call's key tail, the task's sourceAttempt; the attempt is spent once the call answers, kept for the re-issue after a 409
- onWave1:
    switch:
      - reread:    { when: '${ $context.waveOutcome == "reread" }',    then: wave1Reread }
      - exhausted: { when: '${ $context.waveOutcome == "exhausted" }', then: wave1Exhausted }
      - deferred:  { when: '${ ($context.wave1Deferred | length) > 0 }', then: enterDeferral1 }   # a settled success, never a failure (05 §4.5 item 4); due: false until the recorded instant
      - anyFailed: { when: '${ ($context.wave1Failed | length) > 0 }', then: lineFailure1 }
      - accepted:  { then: enterAwaitExpected }
- wave1Reread:                         # 05 §4.5 item 3: read before re-issuing; the read's output is routed, never discarded (D-120)
    timeout: step
    try:
      - call: { step: reconcile-intent }   # body: ref + sweepRound
    catch: *transient
    export: { as: '${ $context + { sweepFailed: .failed, sweepUnresolved: .unresolved, redispatch: .redispatch, sweepAttempt: ($context.sweepRound | tostring), sweepRound: .nextSweepRound } }' }
- onWave1Reread:
    switch:
      - failed: { when: '${ (($context.sweepFailed + $context.sweepUnresolved) | length) > 0 }', then: sweepFailure }
      - again:  { then: awaitReread1 }   # a never-sent row keeps not_found_at, which the same-key re-run sends (05 §4.4)
- awaitReread1:                         # a stage wait is a competing fork with the shared arms (rule 8, D-142)
    fork:
      compete: true
      branches:
        - tick:      { do: [ { waitReread1: { wait: PT30S } }, { arm: { set: { arm: tick } } } ] }   # the same key again after the barrier's poll interval, never at once
        - hold:      { do: [ ‹hold arm of (e)› ] }       # recorded, not waited on: holdPauses is false in every wave wait
        - resume:    { do: [ ‹resume arm of (e)› ] }
        - lifecycle: { do: [ ‹lifecycle arm of (f)› ] }
        - cancel:    { do: [ ‹cancel arm of (d)› ] }
- afterReread1:
    switch:
      - tick:  { when: '${ .arm == "tick" }', then: wave1 }
      - other: { then: leave }          # the stage is re-entered at wave1 (stageLoop), which re-issues the same key
- wave1Exhausted:
    set: { failureScope: line, failureSubjects: '${ [ $context.wave1LineRefs[] | { subjectRef: ., reason: "wave1-create-failed", cause: $context.waveCause } ] }', sourceStep: dispatch-wave1-create, sourceAttempt: '${ $context.wave1Attempt }', nextStage: failure, stageLoop: null }
    then: exit
- enterDeferral1: { set: { wave1LineRefs: '${ $context.wave1Deferred }' } }   # before the wait, so an arm that re-enters at wave1 re-dispatches only the deferred lines
- awaitDeferral1:                       # a stage wait is a competing fork with the shared arms (rule 8, D-142)
    fork:
      compete: true
      branches:
        - tick:      { do: [ { waitDeferral1: { wait: PT1M } }, { arm: { set: { arm: tick, heldTicks: '${ if $context.suspensionRef != null and $context.wave1DeferReason == "held" then ($context.heldTicks // 0) + 1 else 0 end }' } } } ] }   # the re-dispatch is the re-check: before the recorded instant it re-defers with due: false; a deferral for a recorded suspension is a held wait (05 inst-pi-held)
        - hold:      { do: [ ‹hold arm of (e)› ] }       # recorded, not waited on: holdPauses is false in every wave wait
        - resume:    { do: [ ‹resume arm of (e)› ] }
        - lifecycle: { do: [ ‹lifecycle arm of (f)› ] }
        - cancel:    { do: [ ‹cancel arm of (d)› ] }
- afterDeferral1:
    switch:
      - heldPoll: { when: '${ .arm == "tick" and $context.heldTicks >= 15 }', then: pollHeld }   # every 15th PT1M tick (D-133)
      - tick:     { when: '${ .arm == "tick" }', then: wave1Again }
      - other:    { then: leave }       # re-entered at wave1 (stageLoop)
- wave1Again: { set: { wave1LineRefs: '${ $context.wave1Deferred }' }, then: wave1 }
- lineFailure1:                         # fragment (c)
    set: { failureScope: line, failureSubjects: '${ [ $context.wave1Failed[] | { subjectRef: .lineRef, reason: .reason, cause: "explicit-failure-confirmation" } ] }', sourceStep: dispatch-wave1-create, sourceAttempt: '${ $context.wave1Attempt }', nextStage: failure, stageLoop: null }
    then: exit
- enterAwaitExpected: { set: { stageLoop: awaitExpected, holdPauses: false } }
- evaluate:                             # composable (04): both halves from Orders' record — due (database time against expected_fulfillment_at) and every create confirmed
    timeout: step
    try:
      - call: { step: evaluate-activation-eligibility }   # body: ref + planRef, evaluationSeq: $context.barrierSeq; output: due, released, eligibleLineRefs[], pendingLineRefs[], undispatchedLineRefs[], nextEvaluationSeq
    catch: *transient
    export: { as: '${ $context + { expectedDue: .due, released: .released, eligibleLineRefs: .eligibleLineRefs, undispatched: (.undispatchedLineRefs // []), barrierSeq: .nextEvaluationSeq } }' }
- onEvaluate:
    switch:
      - undispatched:  { when: '${ ($context.undispatched | length) > 0 }', then: redispatchUndispatched }   # a pending line with no live wave-1 intent: a retried or rowless line (05 §4.5 item 6, D-119)
      - releasedFirst: { when: '${ $context.released and ($context.spawned | not) }', then: preActivation }
      - releasedAgain: { when: '${ $context.released }', then: wave2 }   # the spawn signal is recorded once
      - notDue:        { when: '${ $context.expectedDue | not }', then: awaitExpected }
      - pending:       { then: enterBarrierLoop }
- awaitExpected:                        # the barrier's timer half: the re-check loop of waitExpected
    fork:
      compete: true
      branches:
        - tick:      { do: [ { waitExpected: { wait: PT1H } }, { arm: { set: { arm: tick, heldTicks: '${ if $context.suspensionRef != null then ($context.heldTicks // 0) + 1 else 0 end }' } } } ] }
        - hold:      { do: [ ‹hold arm of (e)› ] }       # recorded, not waited on: holdPauses is false here
        - resume:    { do: [ ‹resume arm of (e)› ] }
        - lifecycle: { do: [ ‹lifecycle arm of (f)› ] }
        - cancel:    { do: [ ‹cancel arm of (d)› ] }
- afterExpected:
    switch:
      - heldPoll: { when: '${ .arm == "tick" and $context.heldTicks >= 1 }', then: pollHeld }   # every PT1H tick while a suspension is recorded (D-133)
      - tick:  { when: '${ .arm == "tick" }', then: evaluate }
      - other: { then: leave }
- enterBarrierLoop: { set: { stageLoop: barrierLoop } }
- awaitConfirmationOrPoll:              # re-evaluate on EVERY contributing signal: a confirmation OR the poll interval
    fork:
      compete: true
      branches:
        - confirmation:
            do:
              - listenConfirmation:
                  listen:
                    to:
                      any:
                        - with: { type: ‹ProvisioningIntentConfirmed, 05 §3.3› }
                          correlate: { orderId: { from: '${ .data.orderId }', expect: '${ $context.orderId }' }, orderVersion: { from: '${ .data.orderVersion }', expect: '${ $context.orderVersion }' } }
                        - with: { type: ‹ProvisioningIntentFailed, 05 §3.3› }
                          correlate: { orderId: { from: '${ .data.orderId }', expect: '${ $context.orderId }' }, orderVersion: { from: '${ .data.orderVersion }', expect: '${ $context.orderVersion }' } }
                    read: envelope
                  output: { as: '${ .[0] | { lineRef: .data.lineRef, wave: .data.wave } }' }
              - arm: { set: { arm: confirmation, confirmedLineRef: '${ .lineRef }', confirmedWave: '${ .wave }' } }   # 05 §4.5 item 7: the line reference and wave only
        - poll:      { do: [ { waitPoll: { wait: PT30S } }, { arm: { set: { arm: poll, heldTicks: '${ if $context.suspensionRef != null then ($context.heldTicks // 0) + 1 else 0 end }' } } } ] }
        - hold:      { do: [ ‹hold arm of (e)› ] }
        - resume:    { do: [ ‹resume arm of (e)› ] }
        - lifecycle: { do: [ ‹lifecycle arm of (f)› ] }
        - cancel:    { do: [ ‹cancel arm of (d)› ] }
- afterSignal:
    switch:
      - confirmation: { when: '${ .arm == "confirmation" }', then: reconcileHint }
      - heldPoll:     { when: '${ .arm == "poll" and $context.heldTicks >= 30 }', then: pollHeld }   # every 30th PT30S poll while a suspension is recorded (D-133)
      - poll:         { when: '${ .arm == "poll" }',         then: reconcilePoll }
      - other:        { then: leave }
- reconcileHint:                        # composable (05): the confirmation is a wake-up; read the line before re-evaluating
    timeout: step
    try:
      - call: { step: reconcile-intent }   # body: ref + lineRef: $context.confirmedLineRef, wave: $context.confirmedWave, sweepRound
    catch: *transient
    export: { as: '${ $context + { sweepFailed: .failed, sweepUnresolved: .unresolved, redispatch: .redispatch, sweepAttempt: ($context.sweepRound | tostring), sweepRound: .nextSweepRound } }' }
    then: onSweep
- reconcilePoll:                        # composable (05): an early read of the due intents; settles a dead lease through settle-from-lookup in-process
    timeout: step
    try:
      - call: { step: reconcile-intent }   # body: ref + sweepRound
    catch: *transient
    export: { as: '${ $context + { sweepFailed: .failed, sweepUnresolved: .unresolved, redispatch: .redispatch, sweepAttempt: ($context.sweepRound | tostring), sweepRound: .nextSweepRound } }' }
- onSweep:                              # 05 §4.5 item 6: the poll and confirmation arms never discard reconcile-intent's output; a line is listed failed or unresolved once — by the round that records it or, for the worker's in-process trip, by the next definition-called round (D-125) — so failed first never starves redispatch
    switch:
      - failed:      { when: '${ (($context.sweepFailed + $context.sweepUnresolved) | length) > 0 }', then: sweepFailure }
      - redispatch1: { when: '${ [ $context.redispatch[] | select(.wave == "wave1_create") ] | length > 0 }', then: redispatchWave1 }
      - redispatch2: { when: '${ $context.spawned and ([ $context.redispatch[] | select(.wave == "wave2_activate") ] | length > 0) }', then: wave2 }   # only once the spawn signal was sent: spawned is pinned, so no walk reaches wave 2 by this case before the re-check and the signal (D-144)
      - reevaluate:  { then: evaluate }
- sweepFailure:                         # fragment (c): never-dispatched | intent-unresolved | wave failures
    set: { failureScope: line, failureSubjects: '${ [ $context.sweepFailed[] | { subjectRef: .lineRef, reason: .reason, cause: "sweep-discovered-terminal-failure" } ] + [ $context.sweepUnresolved[] | { subjectRef: ., reason: "intent-unresolved", cause: "sweep-floor-reached" } ] }', sourceStep: reconcile-intent, sourceAttempt: '${ $context.sweepAttempt }', nextStage: failure, stageLoop: null }
    then: exit
- redispatchWave1: { set: { wave1LineRefs: '${ [ $context.redispatch[] | select(.wave == "wave1_create") | .lineRef ] }' }, then: enterWave1 }
- redispatchUndispatched: { set: { wave1LineRefs: '${ $context.undispatched }' }, then: enterWave1 }
- preActivation:                        # protected (04): SUB-O5 overlap presence + market + authorization freshness, immediately before the first activation
    timeout: step
    try:
      - call: { step: re-check-pre-activation }   # body: ref + planRef, evaluationSeq: $context.recheckSeq
    catch:
      errors: { with: { type: https://serverlessworkflow.io/spec/1.0.0/errors/communication } }
      when: '${ $error.status as $s | any((429, 503, 504, 409); . == $s) }'
      retry: recheck
    export: { as: '${ $context + { preActivation: .verdict, abortReason: .abortReason, recheckSeq: .nextEvaluationSeq } }' }
- onPreActivation:                      # 04 §4.8 item 4: proceed only by an explicit case
    switch:
      - proceed:         { when: '${ $context.preActivation == "proceed" }', then: spawnSignal }
      - abort:           { when: '${ $context.preActivation == "abort" }', then: preActivationAbort }
      - notDispatchable: { then: enterHeldSpawn }   # on-hold | superseded | terminal: the held-spawn wait's PT5M tick re-runs the re-check, and its resume, lifecycle and cancel arms consume what the operation observed; never the PT30S barrier poll (D-145)
- preActivationAbort: { set: { unwind: failure, nextStage: unwind, stageLoop: null }, then: exit }   # never the failure stage; void wave-1 drafts, fulfillment_failed with the abort reason the fence maps (06 §4.8)
- spawnSignal:                          # protected (05): the first activation intent is the Lifecycle spawn/fencing signal
    timeout: step
    try:
      - call: { step: report-spawn-signal }   # body: ref + round: $context.spawnRound; output: spawnSignal ∈ recorded | already-recorded | held | not-dispatchable, nextRound
    catch: *transient
    export: { as: '${ $context + { spawnAnswer: .spawnSignal, spawnRound: .nextRound } }' }   # no pinned member is written from the answer: spawned is set by the case below (D-144)
- onSpawn:
    switch:
      - held:            { when: '${ $context.spawnAnswer == "held" }', then: enterHeldSpawn }   # Lifecycle not-admissible on an on_hold order (05 §3.3): wait for the resume, then the next round
      - notDispatchable: { when: '${ $context.spawnAnswer == "not-dispatchable" }', then: enterBarrierLoop }   # not-admissible on a terminal order: a cancel committed first (Lifecycle 06 §4.3); the barrier's lifecycle arm consumes OrderCancelled
      - sent:            { then: spawnSent }   # recorded | already-recorded
- spawnSent: { set: { spawned: true, stageLoop: barrierLoop }, then: wave2 }   # spawned is written only here, as a literal (rule 1); the loop leaves the held-spawn wait before wave 2, so an arm taken from a wave-2 deferral re-enters at the barrier, which re-evaluates, and never re-runs the re-check or the spawn signal (D-144, D-145)
- wave2:                                # protected (05): ONE call, eligible lines as references; the draft-liveness re-read is inside
    try:
      - dispatchWave2:
          timeout: wave2
          try:
            - call: { step: dispatch-wave2-activate }   # body: ref + planRef, lineRefs: $context.eligibleLineRefs, dispatchRound: $context.wave2Round, attemptKey: $context.wave2AttemptKey (this wave's own attempt, D-119); output: accepted[], activated[], failed[], pending[], lapsed[], deferred[], due, nextDispatchRound
          catch: *transientNo409
    catch:                              # 409: activation-precondition-unmet, still-processing or idempotency-lease-expired — read, then back to the barrier
      as: waveError
      when: '${ $waveError.status as $s | any((409, 408, 429, 503, 504); . == $s) }'
      do: [ { classify: { set: { waveOutcome: '${ if $waveError.status == 409 then "reread" else "exhausted" end }', waveCause: '${ if $waveError.status == 408 then "step-deadline-exceeded" else "retry-budget-exhausted" end }' } } } ]
    export: { as: '${ $context + { waveOutcome: (.waveOutcome // "answered"), waveCause: (.waveCause // null), wave2Attempt: (($context.wave2Round | tostring) + (if $context.wave2AttemptKey then ":" + ($context.wave2AttemptKey | tostring) else "" end)), wave2AttemptKey: (if (.waveOutcome // "answered") == "answered" then null else $context.wave2AttemptKey end), wave2Failed: ($context.wave2Failed + (.failed // [])), wave2Pending: (.pending // []), lapsed: (.lapsed // []), wave2Deferred: (.deferred // []), wave2DeferReason: (.deferReason // null), wave2Round: (.nextDispatchRound // $context.wave2Round) } }' }
- onWave2:                              # 05 §4.5 items 4–6 and 9
    switch:
      - reread:     { when: '${ $context.waveOutcome == "reread" }',    then: wave2Reread }
      - exhausted:  { when: '${ $context.waveOutcome == "exhausted" }', then: wave2Exhausted }
      - lapsed:     { when: '${ ($context.lapsed | length) > 0 }', then: rebuildLapsed }
      - deferred:   { when: '${ ($context.wave2Deferred | length) > 0 }', then: awaitDeferral2 }
      - anyFailed:  { when: '${ ($context.wave2Failed | length) > 0 }', then: lineFailure2 }   # D-54
      - anyPending: { when: '${ ($context.wave2Pending | length) > 0 }', then: enterBarrierLoop }  # dependents wait for their dependencies; the same conjunction
      - complete:   { then: reportCompleted }                                                     # pending[] and failed[] both empty
- wave2Reread:                         # 05 §4.5 item 3: read, route what it found, then wait in the barrier loop, whose poll re-issues nothing blind (D-120)
    timeout: step
    try:
      - call: { step: reconcile-intent }   # body: ref + sweepRound
    catch: *transient
    export: { as: '${ $context + { sweepFailed: .failed, sweepUnresolved: .unresolved, redispatch: .redispatch, sweepAttempt: ($context.sweepRound | tostring), sweepRound: .nextSweepRound } }' }
- onWave2Reread:
    switch:
      - failed: { when: '${ (($context.sweepFailed + $context.sweepUnresolved) | length) > 0 }', then: sweepFailure }
      - wait:   { then: enterBarrierLoop }   # a never-sent row keeps not_found_at: the next evaluate names a wave-2 line again, and names a wave-1 line in undispatchedLineRefs
- wave2Exhausted:
    set: { failureScope: line, failureSubjects: '${ [ $context.eligibleLineRefs[] | { subjectRef: ., reason: "wave2-activation-failed", cause: $context.waveCause } ] }', sourceStep: dispatch-wave2-activate, sourceAttempt: '${ $context.wave2Attempt }', nextStage: failure, stageLoop: null }
    then: exit
- rebuildLapsed:                        # composable (05): a lapsed draft is rebuilt and goes back through wave 1 and the barrier, never straight to wave 2
    timeout: step
    try:
      - call: { step: rebuild-wave1 }   # body: ref + planRef, lineRefs: $context.lapsed, rebuildRound: $context.rebuildRound (its own round, never wave 1's)
    catch: *transient
    export: { as: '${ $context + { wave1LineRefs: .rebuilt, rebuildRound: .nextRebuildRound } }' }   # wave1Round is written only from dispatch-wave1-create answers
    then: enterWave1
- awaitDeferral2:                       # a stage wait is a competing fork with the shared arms (rule 8, D-142)
    fork:
      compete: true
      branches:
        - tick:      { do: [ { waitDeferral2: { wait: PT1M } }, { arm: { set: { arm: tick, heldTicks: '${ if $context.suspensionRef != null and $context.wave2DeferReason == "held" then ($context.heldTicks // 0) + 1 else 0 end }' } } } ] }
        - hold:      { do: [ ‹hold arm of (e)› ] }       # recorded, not waited on: holdPauses is false in every wave wait
        - resume:    { do: [ ‹resume arm of (e)› ] }
        - lifecycle: { do: [ ‹lifecycle arm of (f)› ] }
        - cancel:    { do: [ ‹cancel arm of (d)› ] }
- afterDeferral2:
    switch:
      - heldPoll: { when: '${ .arm == "tick" and $context.heldTicks >= 15 }', then: pollHeld }   # D-133
      - tick:     { when: '${ .arm == "tick" }', then: wave2Again }
      - other:    { then: leave }       # re-entered by stageLoop (awaitExpected or barrierLoop, never the held-spawn loop, which spawnSent left), which re-evaluates before any dispatch
- wave2Again: { set: { eligibleLineRefs: '${ $context.wave2Deferred }' }, then: wave2 }
- lineFailure2:
    set: { failureScope: line, failureSubjects: '${ [ $context.wave2Failed[] | { subjectRef: .lineRef, reason: .reason, cause: "explicit-failure-confirmation" } ] }', sourceStep: dispatch-wave2-activate, sourceAttempt: '${ $context.wave2Attempt }', nextStage: failure, stageLoop: null }
    then: exit
- reportCompleted:                      # protected (06): in_fulfillment → completed with per-line subscription ids; enqueues OrderFulfillmentCompleted
    timeout: step
    try:
      - call: { step: report-outcome }  # body: ref + outcome: completed, round: $context.reportRound; output: lifecycleCall ∈ acknowledged | held, nextRound
    catch: *transient
    export: { as: '${ $context + { lifecycleCall: .lifecycleCall, reportRound: .nextRound } }' }
- onReport:
    switch:
      - held:     { when: '${ $context.lifecycleCall == "held" }', then: enterHeldReport }   # a completed acknowledgement of an on_hold order (06 §3.3): resume first
      - reported: { then: terminateCompleted }
- terminateCompleted:                   # protected (01); terminationKind: completed
    timeout: step
    try:
      - call: { step: terminate-instance }
    catch: *transient
    then: end
- enterHeldSpawn: { set: { stageLoop: heldSpawn, holdPauses: false }, then: heldWait }   # a held spawn signal or a not-dispatchable re-check: the stage loop, a routing member, names the call to repeat (D-144, D-145)
- enterHeldReport: { set: { stageLoop: heldReport, holdPauses: false } }   # a held completion report
- heldWait:
    fork:
      compete: true
      branches:
        - tick:      { do: [ { waitHeld: { wait: PT5M } }, { arm: { set: { arm: tick, heldTicks: '${ if $context.suspensionRef != null then ($context.heldTicks // 0) + 1 else 0 end }' } } } ] }   # covers a resume consumed before this wait was armed
        - hold:      { do: [ ‹hold arm of (e)› ] }
        - resume:    { do: [ ‹resume arm of (e)› ] }
        - lifecycle: { do: [ ‹lifecycle arm of (f)› ] }
        - cancel:    { do: [ ‹cancel arm of (d)› ] }
- afterHeldWait:
    switch:
      - heldPoll: { when: '${ .arm == "tick" and $context.heldTicks >= 3 }', then: pollHeld }   # D-133
      - tick:  { when: '${ .arm == "tick" }', then: retryHeld }
      - other: { then: leave }          # hold | resume | lifecycle | cancel; a resume returns to retryHeld through enter
- retryHeld:                            # decided by the stage loop alone (rule 1, D-144)
    switch:
      - spawn:  { when: '${ $context.stageLoop == "heldSpawn" }', then: preActivation }   # Lifecycle re-runs the re-check after a hold (Lifecycle 06 §4.3), then the spawn signal under its next round
      - report: { then: reportCompleted }   # stageLoop heldReport
- pollHeld:                             # protected (08): the resume wait's poll, from a fulfillment wait or a held deferral where a hold was only recorded (D-133)
    timeout: step
    try:
      - call: { step: apply-resume }    # body: ref + trigger: poll, suspensionRef, round: $context.resumePollRound; output: resumeOutcome ∈ resumed-by-read | still-held, due, failedTaskRefs[], nextRound
    catch: *transient
    export: { as: '${ $context + { resumeOutcome: .resumeOutcome, resumePollTail: ("poll:" + $context.suspensionRef + ":" + ($context.resumePollRound | tostring)), resumePollRound: .nextRound, resumeFailed: (.failedTaskRefs // []), heldTicks: 0, suspensionRef: (if .resumeOutcome == "resumed-by-read" then null else $context.suspensionRef end) } }' }
- afterHeldPoll:                        # stageLoop first, so no walk takes the failure route before begin-fulfillment; either answer then continues with the tick's own work
    switch:
      - eligibility:      { when: '${ $context.stageLoop == "awaitEligibilityChange" }', then: eligibility }   # before begin-fulfillment no failure is deferred, so failedTaskRefs is empty
      - deferredFailures: { when: '${ ($context.resumeFailed | length) > 0 }', then: heldPollFailure }   # 08 §4.7 item 8: before any dispatch
      - expected:         { when: '${ $context.stageLoop == "awaitExpected" }', then: evaluate }        # also the held wave-2 deferral entered from awaitExpected
      - barrier:          { when: '${ $context.stageLoop == "barrierLoop" }',   then: reconcilePoll }   # also the held wave-2 deferral entered from the barrier
      - held:             { when: '${ $context.stageLoop == "heldSpawn" or $context.stageLoop == "heldReport" }', then: retryHeld }
      - wave1:            { then: wave1Again }   # stageLoop wave1: the held wave-1 deferral
- heldPollFailure:                      # fragment (c); the failing call's key tail is poll:{suspensionRef}:{round}, unique across suspensions (07 §3.3)
    set: { failureScope: line, failureSubjects: '${ [ $context.resumeFailed[] | { subjectRef: .lineRef, reason: .reason, cause: (if .reason == "intent-unresolved" then "sweep-floor-reached" else "sweep-discovered-terminal-failure" end) } ] }', sourceStep: apply-resume, sourceAttempt: '${ $context.resumePollTail }', nextStage: failure, stageLoop: null }
    then: exit
- leave: { set: { nextStage: '${ .arm }', returnStage: fulfillment }, then: exit }
```

The overdue monitor, the `do` list of the top-level `overdueMonitor` branch of fragment (a):

```yaml
- waitOverdue: { wait: PT1H }           # the process deadline: expected_fulfillment_at + the overdue window pinned on the plan (04, D-134), checked by 07; a hold never pauses it
- overdueCheck:                         # composable (07): due only after begin-fulfillment, past the stored deadline, before a settled report-outcome
    try:
      - check:
          timeout: step
          try:
            - call: { step: raise-overdue-escalation }   # body: ref + escalationKind: overdue-fulfillment, round: $context.overdueRound; output: due, raised, nextRound
          catch: *transient
    catch:                              # a failed check is the next tick's to repeat under the same round; nothing routes on it
      as: overdueError
      do: [ { unanswered: { set: { due: false, raised: false } } } ]
    export: { as: '${ $context + { overdueRound: (.nextRound // $context.overdueRound) } }' }
- onOverdue:
    switch:
      - notDue: { when: '${ (.due | not) and (.raised | not) }', then: waitOverdue }   # a settled success of its round; the next tick calls the next round, so the loop outlives the 30-day key lifetime
      - done:   { then: overdueIdle }                                                  # raised once, absorbed, or the order already reported
- overdueIdle: { wait: PT1H, then: overdueIdle }   # the branch never completes, so it never wins the lifetime race
```

**Description**: The fragment reproduces [`04 §3.6`](./04-fulfillment-plan.md#36-interactions--sequences)
*Plan Construction and Freeze*, *Pre-Activation Abort* and *Per-Line Progress*, and
[`05 §3.6`](./05-provisioning-intents.md#36-interactions--sequences) *Two-Wave Provisioning
Dispatch*, *Wave-1 Rebuild* and *Reconciliation Sweep Cycle*, in the order of
[`04 §4.8`](./04-fulfillment-plan.md#48-constraints-this-slice-places-on-the-definition) and
[`05 §4.5`](./05-provisioning-intents.md#45-constraints-this-slice-places-on-the-definition).
**Eligibility.** `evaluate-payment-auth-eligibility` is re-invoked from a competing fork that
carries the admitted `OrderAcceptanceRecorded` `listen`, the `reauthorize-requested` signal, the
`PT5M` poll and the hold, resume, lifecycle and cancel arms; `begin-fulfillment` follows only a
settled `eligible` and a settled `frozen`, receives the `eligible` round as `eligibilitySeq`, and a
`withheld`, `held` or `version-conflict` answer — all settled successes, so no 409 has to be read
as a routing answer — returns to the eligibility wait; once the transition has committed, every
later call answers `in-fulfillment` from Orders' record (`04 §3.6`). Each of the three evaluation operations returns `nextEvaluationSeq` and the
definition passes it back unchanged. **Plan.** A `frozen` plan proceeds; `topology-unavailable`
opens a plan task under either policy; `invalid-graph` opens a plan task under `remediate` and,
under `fail-fast`, passes `begin-fulfillment` before the unwind because no Workflow transition
leaves `approved`. **Waves.** Each wave is **one `call` carrying `lineRefs[]`**; per-line
parallelism and admission are inside the operation (§2.2), and a wave's per-line outcomes come
back as reference lists that route the `switch`. A deferral (`deferred[]`) is a settled success:
the definition waits the fixed `PT1M` of `waitDeferral1` or `waitDeferral2` and calls again under the next
`dispatchRound`. Each of those waits, and the `PT30S` `waitReread1`, is the tick branch of a
competing fork that also carries the hold, resume, lifecycle and cancel arms (decision D-142), so
a cancel or a terminal order event during a long deferral is consumed at once. An arm returns
through the stage's `enter`: wave 1 at `wave1`, which re-dispatches the deferred lines
`enterDeferral1` filed or re-issues the unchanged key, and wave 2 at its stage loop, which
re-evaluates before any dispatch and the operation answers `due: false` and re-defers until the deferral instant
it recorded; `retryAfterMs` is the operation's, never a `wait` value. A `lapsed[]` line goes to
`rebuild-wave1` — under its own `rebuildRound`, never wave 1's round — and back through wave 1 and
the barrier; a `failed[]` line, an exhausted budget
and a spent wave timeout go to fragment (c); a 409 is followed by a `reconcile-intent` read
before the call is re-issued, and that read is routed like the poll arm's: its `failed[]` and
`unresolved[]` go to fragment (c), its round is passed back, and wave 1 is re-issued under the
unchanged key only after the `PT30S` `waitReread1`, while wave 2 waits in the barrier loop
(decision D-120). **Re-dispatch after a retry** (decision D-119). An operator's line retry mints
the line's next wave attempt in Orders' record when its intent is recorded `failed`, and the next
`attempt` of its wave's dispatch family, which the failure stage files by `retryWave` as
`wave1AttemptKey` or `wave2AttemptKey`; each wave call passes only its own and spends it once the
call answers. `evaluate-activation-eligibility` answers `undispatchedLineRefs` — every `pending`
line with no live wave-1 intent under its current attempt, which is a retried wave-1 line, a
line a crashed dispatch never wrote, or a never-sent row — and `onEvaluate` sends them to wave 1
before anything else; a retried wave-2 line is `draft_created` again and returns through
`eligibleLineRefs`. **The barrier is a conjunction and is re-evaluated on every contributing
signal**: `evaluate-activation-eligibility` answers both halves from Orders' record — `due`,
database time against the plan's stored `expected_fulfillment_at`, which `awaitExpected`
re-checks on the `PT1H` `waitExpected` tick, and the all-creates half, which Orders' own
consumer of the Subscriptions outcome events maintains
([`09 §3.6` *System-actor delivery over the event broker*](./09-read-and-authz.md#36-interactions--sequences)),
so the definition never holds the confirmation count itself. Once `due`, `awaitConfirmationOrPoll`
returns to `evaluate` on **either** a confirmation event **or** the `PT30S` poll — each through
`reconcile-intent`, whose `failed[]` and `unresolved[]` route to fragment (c) and whose
`redispatch[]` routes to the named wave — so a confirmation that landed between an evaluation and
the `listen` cannot hang the barrier and the timer is never the sole trigger
([`../ADR/0004`](../ADR/0004-cpt-cf-bss-orders-workflow-adr-two-wave-activation-barrier.md) as
amended). The poll arm is an early read of the schedule the `reconciliation-sweep` worker runs on
`next_sweep_at` for every instance (`01 §3.8`); a hold is recorded here and not waited on, so the
poll, the `waitExpected` tick and the overdue monitor keep running while the dispatch operations
defer on `owf_process_instance.suspended`. While the definition holds a recorded `suspensionRef`,
every wait of this stage — the eligibility, expected-time, barrier and held waits, and a wave
deferral answered `deferReason = held` — calls `pollHeld` on its tick about every `PT15M`
(`heldTicks`, §3.6 *Fixed waits and re-check loops*), which is the resume wait's `apply-resume`
`trigger: poll` (decision D-133). A resume lost between listens is therefore applied from
Lifecycle's order read, and a held deferral cannot loop until the lifetime ceiling even when the
resume it waits for was lost; since D-142 the deferral also carries the resume arm itself. `afterHeldPoll` decides on `stageLoop` first: the eligibility wait precedes
`begin-fulfillment`, so it has no deferred failures and no failure route. Elsewhere a non-empty
`failedTaskRefs[]` goes to fragment (c) before any dispatch, and either answer then runs the tick's
own re-check. `re-check-pre-activation` runs before the first
activation, and again only from the held-spawn wait: `proceed` is the only case that reaches `report-spawn-signal`, `abort` goes to the
unwind and `not-dispatchable` — the order `on_hold`, superseded or terminal — waits in the
held-spawn wait, whose `PT5M` tick runs the re-check again and whose resume, lifecycle and cancel
arms consume what it observed, never the `PT30S` barrier poll (decision D-145). **Held.** A `report-spawn-signal` or a
completion `report-outcome` that Lifecycle refuses `not-admissible` because the order is `on_hold`
answers `held`, a settled success; the stage waits in `heldWait` for the resume (or the `PT5M`
tick) and calls the same operation again under the next round — for the spawn signal after
`re-check-pre-activation` runs again, as Lifecycle prescribes after a hold — because Lifecycle replays a
refusal under its key and only a new round reaches it after the resume. The stage loop names the
call to repeat — `heldSpawn` or `heldReport`, literals of a routing member — so the walk of §2.2
rule 1 decides it; a sent spawn signal (`spawnSent`) sets `spawned`, a pinned literal, and leaves
the held-spawn loop for `barrierLoop` before wave 2, so an arm taken from a wave-2 deferral
re-enters at the barrier's `evaluate`, never at the re-check (decisions D-144, D-145)
([`01 §3.3` *Rounds and attempts*](./01-foundation.md#rounds-and-attempts-the-one-rule-for-re-invokable-operations)). A spawn signal Lifecycle refuses `not-admissible` for an order it holds
terminal answers `not-dispatchable`: a cancel committed before the signal, which Lifecycle
declares the normal outcome of the race ("if cancellation commits first, the signal refuses and
Workflow dispatches nothing", [Lifecycle `06 §4.3`](../../../orders-lifecycle/docs/design/06-workflow-seam.md#43-begin-fulfillment-and-the-spawn-signal-normative)),
and the stage returns to the barrier loop, whose lifecycle arm consumes `OrderCancelled` and
unwinds on the terminal-event path (decision D-109). **Plan failure.** An exhausted plan task
re-enters this stage at `planFailFast`, and a `planFailFast` whose `begin-fulfillment` answers
`withheld`, `held` or `version-conflict` waits in the eligibility fork with `planFailed` set, so
the next `eligible` round passes `begin-fulfillment` under a new `eligibilitySeq` before the
unwind; an amendment or Lifecycle's `approved` expiry ends the wait through the lifecycle arm (`04 §4.3`).
**Overdue.** The overdue window is the
top-level `overdueMonitor` branch: every `PT1H` it calls `raise-overdue-escalation` with
`escalationKind: overdue-fulfillment` under the round the previous tick returned, which answers
`due: false`, records nothing and settles its round until database time passes `expected_fulfillment_at` + the plan's pinned overdue window (default 24 h, D-134), answers `raised: false`
once `report-outcome` has settled, and otherwise records the escalation once. It raises an
escalation and nothing else, never cancels fulfillment and is never paused by a hold
([`07`](./07-manual-tasks.md), `cpt-cf-bss-orders-workflow-fr-owf-overdue-escalation`); it stops
with the invocation, so it needs no `listen` for Orders' own terminal events.

#### (c) Partial failure: manual task, resume or compensate

**ID**: `cpt-cf-bss-orders-workflow-seq-def-partial-failure`

**Use cases**: `cpt-cf-bss-orders-workflow-fr-owf-retry`, `cpt-cf-bss-orders-workflow-fr-owf-manual-task`, `cpt-cf-bss-orders-workflow-fr-owf-override-semantics`, `cpt-cf-bss-orders-workflow-fr-owf-compensation-execution`

**Actors**: `cpt-cf-bss-orders-workflow-actor-owf-fulfillment-operator`, `cpt-cf-bss-orders-workflow-actor-owf-subscriptions`, `cpt-cf-bss-orders-workflow-actor-owf-orders-lifecycle`

The failure stage, the `do` list of `process.failure` — entered with the four members of the
`create-manual-task` body in `$context`, which every entering branch sets (decision D-116):
`failureScope`; `failureSubjects`, one `{ subjectRef, reason, cause }` per subject, with `reason`
and `cause` the `failure_reason` and `failure_cause` enums of `07 §3.7`; `sourceStep`; and
`sourceAttempt`, the failing call's key tail `{round}[:{attempt}]` (`01 §3.3` *Rounds and
attempts*; `resumeEventId` for `apply-resume`, which is keyed by its event, and
`poll:{suspensionRef}:{round}` for its poll, whose round family is per suspension) — and, for an
order-scope task, `taskReturnStage` and `taskReturnLoop`. `failureScope` is one of the literals
`line`, `plan`, `order` and is a pinned member of §2.2 rule 1: the stage routes a retry and an
exhaustion on it, never on the answer's `resumeAt` or a returned list (decision D-144):

```yaml
- enter:
    switch:
      - waiting:   { when: '${ $context.stageLoop == "awaitResolution" }', then: awaitResolution }
      - resolving: { when: '${ $context.stageLoop == "resolveTask" }',     then: resolveTask }   # a consumed task resolution is re-issued under its unchanged key, never dropped (D-147)
      - fresh:     { then: onPolicy }
- onPolicy:                             # 07 §4.8 item 1
    switch:
      - remediate: { when: '${ $context.policy == "remediate" or $context.forceTask }', then: createTasks }
      - failFast:  { then: failFastUnwind }   # fail-fast records a tracked incident inside compensate-order's path; no actionable task
- failFastUnwind:                       # every route to a failure unwind: a failure is acknowledged only from fulfillment (06 §3.2, D-109); decided on pinned members only (D-144)
    switch:
      - begun:    { when: '${ $context.beginResult == "in-fulfillment" }', then: toFailureUnwind }
      - notBegun: { when: '${ $context.failureScope == "plan" }', then: toBeginFirst }   # only a plan-scope failure is reached before begin-fulfillment (07 §4.2)
      - never:    { then: orderTaskExhausted }
- toBeginFirst: { set: { nextStage: fulfillment, stageLoop: planFailFast }, then: exit }   # 04 §4.3: pass begin-fulfillment, then the unwind
- toFailureUnwind: { set: { unwind: failure, nextStage: unwind, stageLoop: null }, then: exit }
- orderTaskExhausted:                   # an order-scope task escalates and is never exhausted (07 §4.2): a defect of the version or the operation, never a business outcome, so it faults the invocation, as unknownStage does
    raise:
      error:
        type: https://serverlessworkflow.io/spec/1.0.0/errors/runtime
        status: 500
        title: order-task-exhausted
        detail: an order-scope manual task answered exhausted before begin-fulfillment
- createTasks:                          # protected (07): exactly one actionable task per failed subject, reopen semantics inside
    timeout: step
    try:
      - call: { step: create-manual-task }   # body: ref + scope: $context.failureScope, subjects: $context.failureSubjects, sourceStep: $context.sourceStep, sourceAttempt: $context.sourceAttempt
    catch: *transient
    export: { as: '${ $context + { taskRefs: .taskRefs, exhaustedTaskRefs: .exhaustedTaskRefs, slaRound: .slaRound, wave1Failed: [], wave2Failed: [], forceTask: false } }' }
- onCreate:
    switch:
      - exhausted: { when: '${ ($context.exhaustedTaskRefs | length) > 0 }', then: failFastUnwind }   # the third failed attempt (07 §4.2)
      - open:      { then: enterAwaitResolution }
- enterAwaitResolution: { set: { stageLoop: awaitResolution, holdPauses: false } }
- awaitResolution:                      # 07 §4.8 item 3: every task has a waiter
    fork:
      compete: true
      branches:
        - resolution:                   # the task-resolution arm, also used by (c) awaitCompensationResolution and (d) awaitOperatorAfterPark
            do:
              - listenResolution:
                  listen:
                    to:
                      one:
                        with: { type: gts.cf.core.events.event.v1~cf.bss.orders_workflow.signal.v1~cf.bss.orders_workflow.task_resolution_requested.v1~ }
                        correlate:
                          orderId:      { from: '${ .data.orderId }',      expect: '${ $context.orderId }' }
                          orderVersion: { from: '${ .data.orderVersion }', expect: '${ $context.orderVersion }' }
                    read: envelope
                  output: { as: '${ .[0] | { taskRef: .data.taskRef, requestRef: .data.requestRef } }' }
              - arm: { set: { arm: resolution, taskRef: '${ .taskRef }', requestRef: '${ .requestRef }' } }
        - sla:       { do: [ { waitSla: { wait: PT5M } }, { arm: { set: { arm: sla } } } ] }
        - hold:      { do: [ ‹hold arm of (e)› ] }
        - resume:    { do: [ ‹resume arm of (e)› ] }
        - lifecycle: { do: [ ‹lifecycle arm of (f)› ] }
        - cancel:    { do: [ ‹cancel arm of (d)› ] }
- afterResolution:
    switch:
      - resolution: { when: '${ .arm == "resolution" }', then: enterResolveTask }
      - sla:        { when: '${ .arm == "sla" }',        then: slaCheck }
      - other:      { then: leave }
- enterResolveTask: { set: { stageLoop: resolveTask } }   # the resolution arm consumed its signal: a checkpoint before the call (D-147)
- resolveTask:                          # composable (07)
    timeout: step
    try:
      - call: { step: resolve-manual-task }   # body: ref + trigger: request, taskRef, requestRef
    catch: *transient
    export: { as: '${ $context + { resolution: .resolution, attemptKey: (if .retryWave then $context.attemptKey else .attemptKey end), wave1AttemptKey: (if .retryWave == "wave1_create" then .attemptKey else $context.wave1AttemptKey end), wave2AttemptKey: (if .retryWave == "wave2_activate" then .attemptKey else $context.wave2AttemptKey end), openTaskCount: .openTaskCount, slaRound: .slaRound } }' }   # a line retry's attempt belongs to its wave's dispatch family (retryWave); the latest per wave is that family's counter, so several retries under the remediation hold keep one member per wave (D-117, D-119)
    then: onResolution
- slaCheck:                             # composable (07): the SLA re-check; escalates inside the operation only once the stored SLA deadline has passed
    timeout: step
    try:
      - call: { step: resolve-manual-task }   # body: ref + trigger: sla-check, slaRound: $context.slaRound
    catch: *transient
    export: { as: '${ $context + { resolution: .resolution, slaRound: .slaRound } }' }
- onResolution:                         # 07 §4.8 item 4
    switch:
      - exhausted: { when: '${ $context.resolution == "exhausted" }', then: failFastUnwind }
      - override:  { when: '${ $context.resolution == "override" }',  then: verifyOverride }
      - retry:     { when: '${ $context.resolution == "retry" }',     then: routeRetry }
      - lastClosed: { when: '${ $context.resolution == "closed" and $context.openTaskCount == 0 and $context.failureScope == "line" and $context.beginResult == "in-fulfillment" }', then: toBarrier }   # the last open task closed without a retry: every line an earlier retry held behind the remediation hold returns to the barrier with it (D-146)
      - wait:      { then: enterAwaitResolution }   # closed with a task still open | escalated | refused | none: back to the waiting fork
- routeRetry:                           # by the entry's failureScope, a pinned member, which 07 maps one-to-one onto resumeAt (line → barrier, plan → plan, order → stage); carrying attemptKey (D-144)
    switch:
      - barrier:      { when: '${ $context.failureScope == "line" and $context.openTaskCount == 0 and $context.beginResult == "in-fulfillment" }', then: toBarrier }   # beginResult is pinned: no walk reaches the barrier without begin-fulfillment (D-143)
      - siblingsOpen: { when: '${ $context.failureScope == "line" }', then: enterAwaitResolution }   # 07 §4.3 remediation hold: dispatch waits until the order's last open task resolves (D-117)
      - plan:         { when: '${ $context.failureScope == "plan" }', then: toPlan }
      - stage:        { then: retryStage }   # failureScope order: the stage whose operation failed
- toBarrier: { set: { nextStage: fulfillment, stageLoop: barrierLoop }, then: exit }   # evaluate names every retried wave-1 line in undispatchedLineRefs and every retried wave-2 line in eligibleLineRefs; each is sent under its line's new wave attempt, the call under its wave's attemptKey (D-119)
- toPlan: { set: { nextStage: fulfillment, stageLoop: freezePlan, planAttempt: '${ $context.attemptKey }' }, then: exit }   # a new attempt of construct-and-freeze-plan
- retryStage: { set: { nextStage: '${ $context.taskReturnStage }', stageLoop: '${ $context.taskReturnLoop }' }, then: exit }   # reflect-verdict → approval at reflectVerdict, the one order-scope task the definition creates (D-114)
- verifyOverride:                       # composable (07): follows only a resolve-manual-task that answered override for the same requestRef
    timeout: step
    try:
      - call: { step: verify-override } # body: ref + taskRef, requestRef; output: verified, rejection, exhausted, openTaskCount
    catch: *transient
    export: { as: '${ $context + { overrideVerified: .verified, overrideExhausted: .exhausted, openTaskCount: .openTaskCount } }' }
- afterOverride:
    switch:
      - verified:  { when: '${ $context.overrideVerified and $context.openTaskCount == 0 and $context.beginResult == "in-fulfillment" }', then: toBarrier }   # the line is activated; the conjunction decides whether the order completes (D-143: never before begin-fulfillment)
      - held:      { when: '${ $context.overrideVerified }', then: enterAwaitResolution }                        # siblings still open: the remediation hold holds (D-117)
      - exhausted: { when: '${ $context.overrideExhausted }', then: failFastUnwind }
      - rejected:  { then: enterAwaitResolution }                                 # the task remains open
- leave: { set: { nextStage: '${ .arm }', returnStage: failure }, then: exit }
```

The unwind stage, the `do` list of `process.unwind` — the order of `06 §4.7`: fence **<**
`compensate-order` **<** `report-outcome` **<** `terminate-instance`, entered with
`unwind` ∈ `failure` · `cancel` · `supersede` · `terminal-event` in `$context`, the trigger the
fence is presented. `reportAs` and `terminationKind` are derived from the fence's
`effectiveTrigger`, never from the entry, because a trigger absorbed against a running unwind
keeps that run's report (`06 §4.3`; a cancel taken during a supersede unwind stays a supersede
report):

```yaml
- enter:
    switch:
      - waiting:    { when: '${ $context.stageLoop == "awaitCompensationResolution" }', then: awaitCompensationResolution }
      - resolving:  { when: '${ $context.stageLoop == "resolveCompensationTask" }', then: resolveCompensationTask }   # D-147
      - compensate: { when: '${ $context.stageLoop == "compensate" }', then: compensate }
      - fresh:      { then: fence }
- fence:                                # protected (06): claims or absorbs, promotes failure → cancel (06 §4.3); a permanent failure faults the invocation
    timeout: step
    try:
      - call: { step: run-cancellation-fence }   # body: ref + trigger: $context.unwind, cancelRequestRef, triggerEventId; no failure reason (06 §4.8); key ends in the request ref (06 §4.3), so a second cancel is absorbed, not a key conflict
    catch: *transient
    export: { as: '${ $context + { pass: ($context.pass // 1), effectiveTrigger: .effectiveTrigger, reportAs: ({ "failure": "failed", "cancel": "cancelled", "supersede": "superseded", "terminal-event": "terminal-event" }[.effectiveTrigger]), terminationKind: ({ "failure": "compensated", "cancel": "compensated", "supersede": "superseded", "terminal-event": "terminal-order-event" }[.effectiveTrigger]) } }' }
- enterCompensate: { set: { stageLoop: compensate } }
- compensate:                           # protected (06): the whole reverse walk, ONE operation because the ordinal is Orders'
    try:
      - walk:
          timeout: step
          try:
            - call: { step: compensate-order }   # body: ref + pass: $context.pass; output: compensationState ∈ complete | in-progress | pending-escalation, nextPass, taskRefs[]
          catch: *transient
    catch:                              # the budget or the timeout spent: the next pass waits in awaitCompensationResolution (06 §4.7 item 5); no type filter, so the timeout error (…/errors/timeout, 408) reaches `when`, as in the wave catches
      when: '${ $error.status as $s | any((408, 429, 503, 504, 409); . == $s) }'
      do: [ { exhausted: { set: { compensationState: retry-exhausted } } } ]
    export: { as: '${ $context + { compensationState: .compensationState, pass: (.nextPass // ($context.pass + 1)) } }' }
- onCompensation:                       # 06 §4.7 item 2: only complete reaches report-outcome
    switch:
      - complete:   { when: '${ $context.compensationState == "complete" }', then: reportOutcome }
      - inProgress: { when: '${ $context.compensationState == "in-progress" }', then: awaitRepoll }
      - pending:    { then: enterAwaitCompensationResolution }   # pending-escalation | retry-exhausted: compensate-order opened its task; the order stays non-terminal
- awaitRepoll:                          # an unwind fork: the cancel arm and no other (06 §4.7 item 7, rule 8, D-142)
    fork:
      compete: true
      branches:
        - tick:   { do: [ { waitRepoll: { wait: PT30S } }, { arm: { set: { arm: tick } } } ] }   # the barrier's poll interval; well inside the sweep floor
        - cancel: { do: [ ‹cancel arm of (d)› ] }   # authorize-cancel, then the fence absorbs and promotes, then compensate (stageLoop)
- afterRepoll:
    switch:
      - tick:  { when: '${ .arm == "tick" }', then: compensate }
      - other: { then: leave }
- enterAwaitCompensationResolution: { set: { stageLoop: awaitCompensationResolution, holdPauses: false } }
- awaitCompensationResolution:          # no hold arm: a hold does not pause an unwind (06 §4.7 item 7)
    fork:
      compete: true
      branches:
        - resolution: { do: [ ‹task-resolution arm of awaitResolution› ] }   # sets arm: resolution, taskRef, requestRef
        - sla:        { do: [ { waitSla: { wait: PT5M } }, { arm: { set: { arm: sla } } } ] }
        - retryLeg:   { do: [ { waitRetryLeg: { wait: PT1H } }, { arm: { set: { arm: retry } } } ] }   # inside the sweep floor
        - cancel:     { do: [ ‹cancel arm of (d)› ] }   # authorize-cancel, then the fence absorbs and promotes, then compensate
- afterCompensationResolution:
    switch:
      - resolution: { when: '${ .arm == "resolution" }', then: enterResolveCompensationTask }
      - sla:        { when: '${ .arm == "sla" }',        then: slaCheckUnwind }
      - retry:      { when: '${ .arm == "retry" }',      then: compensate }    # the next pass; already-settled legs are absorbed
      - other:      { then: leave }                                          # cancel
- slaCheckUnwind:
    timeout: step
    try:
      - call: { step: resolve-manual-task }   # body: ref + trigger: sla-check, slaRound: $context.slaRound
    catch: *transient
    export: { as: '${ $context + { slaRound: .slaRound } }' }
    then: awaitCompensationResolution
- enterResolveCompensationTask: { set: { stageLoop: resolveCompensationTask } }   # a checkpoint before the call that consumes the arm's signal (D-147)
- resolveCompensationTask:
    timeout: step
    try:
      - call: { step: resolve-manual-task }   # body: ref + trigger: request, taskRef, requestRef
    catch: *transient
    export: { as: '${ $context + { resolution: .resolution, attemptKey: .attemptKey } }' }
- onCompensationResolution:
    switch:
      - again: { when: '${ $context.resolution == "retry" or $context.resolution == "exhausted" }', then: enterCompensate }   # resumeAt: compensation
      - wait:  { then: enterAwaitCompensationResolution }                                                                 # closed | escalated | refused | none
- reportOutcome:                        # protected (06): the sole Lifecycle outcome caller; outcome-not-reportable or fence-not-claimed fault the invocation (§4.6)
    timeout: step
    try:
      - call: { step: report-outcome }  # body: ref + outcome: $context.reportAs ∈ failed | cancelled | superseded | terminal-event, from the fence's effectiveTrigger, round: ($context.reportRound // 0); reportedOutcome is terminal-event where Lifecycle already holds the order terminal (06 §4.9)
    catch: *transient
    export: { as: '${ $context + { reportRound: .nextRound } }' }   # the family's round carries over from a held completion report; held cannot arise here (06 §4.9: rows 26 and 27 admit failed and cancelled from a held order), so no switch follows
- terminateAborted:                     # protected (01); body: ref + terminationKind: $context.terminationKind, supersededByOrderVersion: $context.supersededByOrderVersion (admit-trigger's currentOrderVersion on a supersede admission, null otherwise, D-155)
    timeout: step
    try:
      - call: { step: terminate-instance }
    catch: *transient
    then: end
- leave: { set: { nextStage: '${ .arm }', returnStage: unwind }, then: exit }
```

**Description**: The fragment reproduces [`06 §3.6`](./06-saga-and-compensation.md#36-interactions--sequences)
*Order-Level Failure Compensation* and *Compensation Walk Order*, and
[`07 §3.6`](./07-manual-tasks.md#36-interactions--sequences) *Manual Task Created on Failure* and
*Override Rejected Without Verified Subscription*. Retry exhaustion is the definition's — the
`catch.retry` on a wave call, and the wave's timeout — and what follows it is
`create-manual-task`, exactly as
[`08 §3.6` *Transient Outage: Retry-Then-Manual-Task*](./08-hold-and-cancel.md#36-interactions--sequences)
draws it; the envelope creates no task (`01 §4.5`). **Every task has a waiter**: `awaitResolution`
and `awaitCompensationResolution` compete the `task-resolution-requested` `listen` with the
`PT5M` `waitSla` tick, which calls `resolve-manual-task` with `trigger: sla-check`; the operation
escalates only once the task's stored SLA deadline has passed in database time, so the tick is a
re-check and no remainder is carried. A non-empty `exhaustedTaskRefs`, an `exhausted` resolution
and an `exhausted: true` override route to the unwind — through `begin-fulfillment` first for a
plan-scope failure, and never for an order-scope task, which `07 §4.2` never exhausts, so that
answer faults the invocation as a defect; a `retry` routes by the entry's `failureScope`, which
`07` maps one-to-one onto the answer's `resumeAt` — `line` to the barrier, `plan` to a new
`construct-and-freeze-plan` attempt, `order` back to the `taskReturnStage` and `taskReturnLoop`
the failing stage recorded — and carries `attemptKey`, which a line retry files per wave as
`wave1AttemptKey` or `wave2AttemptKey` (D-119). A compensation task's retry is the unwind's
(`awaitCompensationResolution`), never this stage's. A `closed` answer that leaves no open task
of a line entry returns to the barrier like the last retry (decision D-146); `closed` with a task
still open, `escalated`, `refused` and `none` return to the waiting fork. **The reverse walk is one operation**:
`compensate-order` owns `compensation_sequence`, the descending walk, the per-subject phase
resolution and the `failed-pending-escalation` records
([`../ADR/0005`](../ADR/0005-cpt-cf-bss-orders-workflow-adr-saga-compensable-no-pivot.md) as
amended), because splitting the walk into definition tasks would put the ordinal — a fact
Orders' saga log is authoritative for — into engine state. The definition keeps only `pass`, the
walk's resumption counter, and presents a greater `pass` on every re-invocation. `in-progress` is
re-polled on the `PT30S` interval; `pending-escalation` and an exhausted budget wait in
`awaitCompensationResolution`, which carries the cancel arm (a cancel during an unwind is
authorized, then absorbed and promoted by the fence, then the walk continues) and no hold arm. A
permanent refusal of `run-cancellation-fence` or `report-outcome` is not caught: it faults the
invocation, which the platform records as `failed` and, with no function-level handler,
`dead_lettered` (§3.3 *Generic control*), and which the instance liveness pass raises as the
`invocation-dead` task (§4.6, decision D-114). The unwind's `report-outcome` carries the
family's round, which a held completion report may already have advanced; it needs no `held`
case. **One failure rule.** A retry budget spent, a timeout, or a permanent refusal faults the
invocation, except where this fragment catches it because the failure is a subject an
operator can act on: a wave call's exhaustion and a wave's `failed[]` (line tasks),
`compensate-order`'s exhaustion (the next pass, after `awaitCompensationResolution`) and
`reflect-verdict`'s refusal (an order-scope task). **The remediation hold holds**: a `retry`
or a verified override of one line while the order still has open tasks
(`openTaskCount > 0`) returns to `awaitResolution`, where every open task keeps its waiter, and
the last resolution — a retry, a verified override or a Seller Operator's `closed` — takes every
retried line back to the barrier together (`07 §4.3`, decisions D-117, D-146). The one
order-scope task this stage creates, `approval-reflection-refused`, offers no task `cancel`
(`07 §4.4`), because closing it would leave the stage with nothing to wait on: before fulfillment
the order is ended by Lifecycle's own cancel, whose `OrderCancelled` the lifecycle arm consumes
(D-109, D-146). `report-outcome` is called only when `compensate-order`
reports `complete`, which is the PRD's "order remains non-terminal until operational compensation
reaches a known outcome".

#### (d) Cancel

**ID**: `cpt-cf-bss-orders-workflow-seq-def-cancel`

**Use cases**: `cpt-cf-bss-orders-workflow-fr-owf-compensation-execution`, `cpt-cf-bss-orders-workflow-fr-owf-terminal-order-events`

**Actors**: `cpt-cf-bss-orders-workflow-actor-owf-seller-operator`, `cpt-cf-bss-orders-workflow-actor-owf-orders-lifecycle`, `cpt-cf-bss-orders-workflow-actor-owf-subscriptions`

```yaml
# the cancel arm of every stage fork: it only listens
- awaitCancel:
    listen:
      to:
        one:
          with: { type: gts.cf.core.events.event.v1~cf.bss.orders_workflow.signal.v1~cf.bss.orders_workflow.cancel_requested.v1~ }
          correlate:
            orderId:      { from: '${ .data.orderId }',      expect: '${ $context.orderId }' }
            orderVersion: { from: '${ .data.orderVersion }', expect: '${ $context.orderVersion }' }
      read: envelope
    output: { as: '${ .[0] | { requestRef: .data.requestRef } }' }
- arm: { set: { arm: cancel, cancelRequestRef: '${ .requestRef }' } }
```

The cancel stage, the `do` list of `process.cancel`, entered with `returnStage` and `stageLoop`
of the stage the cancel was taken from:

```yaml
- authorize:                            # protected (08): the apply-time re-check of authority (09 §4.4); precedes the fence on every cancel path
    timeout: step
    try:
      - call: { step: authorize-cancel }   # body: ref + cancelRequestRef; output: authorized (bool), preFulfillment (bool)
    catch: *transient                   # the budget or the timeout spent faults the invocation (§4.6, D-114): the re-drive resumes here under the open key, and the fallback is this cancel
    export: { as: '${ $context + { cancelAuthorized: .authorized } }' }
- onAuthorize:
    switch:
      - authorized: { when: '${ $context.cancelAuthorized == true }', then: toCancelUnwind }
      - denied:     { then: back }      # the stage and loop the cancel was taken from — the resume wait when taken from a hold (08 §4.7 item 5): a withdrawn authority, refused and audited with no task (D-115), or preFulfillment (08 §3.6, D-109): the order is cancelled through Lifecycle and the lifecycle arm ends the process
- toCancelUnwind: { set: { unwind: cancel, nextStage: unwind, stageLoop: null }, then: exit }   # fragment (c): the fence claims, or absorbs against a running unwind and promotes only a failure run (06 §4.3); reportAs follows its effectiveTrigger
- back: { set: { nextStage: '${ $context.returnStage }' }, then: exit }
```

The ceiling stage, the `do` list of `process.ceiling`, entered from `ceilingEntry` of fragment (a):

```yaml
- enter:
    switch:
      - parked:    { when: '${ $context.stageLoop == "awaitOperatorAfterPark" }', then: awaitOperatorAfterPark }
      - resolving: { when: '${ $context.stageLoop == "resolveCeilingTask" }',     then: resolveCeilingTask }   # D-147
      - fresh:     { then: escalateCeiling }
- escalateCeiling:                      # 07 §4.8 item 7: the escalation before the park; it opens this ceiling's lifetime-ceiling-reached order task
    timeout: step
    try:
      - call: { step: raise-overdue-escalation }   # body: ref + escalationKind: lifetime-ceiling, round: $context.ceilingRound; output: taskRef, nextRound
    catch: *transient
    export: { as: '${ $context + { ceilingTaskRef: .taskRef, ceilingSubject: ("ceiling:" + ($context.ceilingRound | tostring)), ceilingRound: .nextRound, ceilingSlaRound: 0 } }' }   # each ceiling is its own round: a new escalation, task and park subject (D-121); its task's SLA family starts at round 0 (D-129)
- parkCeiling:                          # from started or suspended only (01 §3.7): afterLifetime routes neither an unwind nor the verdict park here
    timeout: step
    try:
      - call: { step: park }            # body: ref + parkReason: lifetime-ceiling, subjectRef: $context.ceilingSubject
    catch: *transient
- enterOperatorWait: { set: { stageLoop: awaitOperatorAfterPark } }
- awaitOperatorAfterPark:               # no hold arm; no unpark-requested arm until Q-13 gives it an origin route (D-122)
    fork:
      compete: true
      branches:
        - resolution:                   # this ceiling's task only: 07 §4.4 offers no action on another task while it is open, so no other resolution is consumed here
            do:
              - listenCeilingResolution:
                  listen:
                    to:
                      one:
                        with: { type: gts.cf.core.events.event.v1~cf.bss.orders_workflow.signal.v1~cf.bss.orders_workflow.task_resolution_requested.v1~ }
                        correlate:
                          orderId:      { from: '${ .data.orderId }',      expect: '${ $context.orderId }' }
                          orderVersion: { from: '${ .data.orderVersion }', expect: '${ $context.orderVersion }' }
                          taskRef:      { from: '${ .data.taskRef }',      expect: '${ $context.ceilingTaskRef }' }
                    read: envelope
                  output: { as: '${ .[0] | { requestRef: .data.requestRef } }' }
              - arm: { set: { arm: resolution, requestRef: '${ .requestRef }' } }
        - sla:       { do: [ { waitSla: { wait: PT5M } }, { arm: { set: { arm: sla } } } ] }   # 07 §4.8 item 3: the ceiling task has a waiter and an SLA tick like every task (D-129)
        - lifecycle: { do: [ ‹lifecycle arm of (f)› ] }
        - cancel:    { do: [ ‹cancel arm of (d)› ] }   # the Seller Operator's way to end a ceiling park: the order cancel, not the task's cancel (07 §4.4, D-129)
- afterOperatorPark:
    switch:
      - resolution: { when: '${ .arm == "resolution" }', then: enterResolveCeilingTask }
      - sla:        { when: '${ .arm == "sla" }',        then: slaCheckCeiling }
      - other:      { then: leave }     # cancel: parked → compensating through the fence (01 §3.7); lifecycle
- slaCheckCeiling:                      # composable (07): escalates the ceiling task to the Seller Operator once its stored deadline has passed; checks no other task, so it answers escalated or none, never exhausted
    timeout: step
    try:
      - call: { step: resolve-manual-task }   # body: ref + trigger: sla-check, taskRef: $context.ceilingTaskRef, slaRound: $context.ceilingSlaRound
    catch: *transient
    export: { as: '${ $context + { ceilingSlaRound: .slaRound } }' }
    then: awaitOperatorAfterPark
- enterResolveCeilingTask: { set: { stageLoop: resolveCeilingTask } }   # a checkpoint before the call that consumes the arm's signal (D-147)
- resolveCeilingTask:                   # composable (07)
    timeout: step
    try:
      - call: { step: resolve-manual-task }   # body: ref + trigger: request, taskRef: $context.ceilingTaskRef, requestRef
    catch: *transient
    export: { as: '${ $context + { resolution: .resolution } }' }
- onCeilingResolution:
    switch:
      - retry: { when: '${ $context.resolution == "retry" }', then: unparkAfterCeiling }
      - wait:  { then: enterOperatorWait }   # refused (the task's cancel is not offered, 07 §4.4) | closed | none
- unparkAfterCeiling:
    timeout: step
    try:
      - call: { step: unpark }          # body: ref + subjectRef: $context.ceilingSubject; refused not-found unless this ceiling's task is resolved retry (01 §3.3); output: phase = the pre-park phase
    catch: *transient
- backToProcess: { set: { nextStage: '${ $context.ceilingReturnStage }', stageLoop: '${ $context.ceilingReturnLoop }', returnStage: '${ $context.ceilingReturnBack }' }, then: exit }   # the routing state the ceiling interrupted, returnStage included, so a lifecycle, hold, resume or cancel stage it interrupted goes back where it would have (D-161)
- leave: { set: { nextStage: '${ .arm }', returnStage: ceiling }, then: exit }
```

**Description**: The fragment reproduces [`06 §3.6`](./06-saga-and-compensation.md#36-interactions--sequences)
*Authorized-Cancellation Compensation* and *Five-Step Cancellation Fencing* and
[`08 §3.6` *Workflow-Mediated Cancel with Compensation Evidence*](./08-hold-and-cancel.md#36-interactions--sequences).
The cancel is a signal (§3.3) because the platform's generic `cancel` would end the invocation
without the fence; the arm exists in every stage `fork` of the process so a cancel is consumable
in every stage, and every stage's `leave` routes it to the one cancel stage. `authorize-cancel`
is where the request's authority is re-checked at apply time — fencing can outlive the request
by days — and precedes `run-cancellation-fence` on every cancel path; a denied re-check returns
through `back` to the stage and loop the cancel was taken from, which is the resume wait
(`awaitResume`) when the cancel was taken during a hold, because the instance is still
`suspended`. A cancel of an order whose fulfillment has not begun has no Workflow seam: the
cancel route refuses it and `authorize-cancel` answers `preFulfillment` if one reaches it, so it
also returns through `back`, and the order is cancelled through Lifecycle's own cancel, whose
`OrderCancelled` the lifecycle arm consumes (decision D-109). The approval and eligibility forks
keep their cancel arm for a request accepted against an order Lifecycle already holds terminal. A
withdrawn authority is refused and audited on the request and raises no task, because nothing
waits on it: the process continues where the cancel was taken, and a Seller Operator who still
means it submits a new cancel under a fresh authorization (decision D-115). A spent budget of
`authorize-cancel` faults the invocation (§4.6): the platform re-drive resumes at
`authorize-cancel` under its still-open key, and the `invocation-dead` fallback is the cancel
itself (`01 §4.16`, decision D-114). A parked instance — including one parked at the
lifetime ceiling — reaches an unwind only through this path and the fence
(`parked → compensating`, `01 §3.7`). **After the ceiling** the instance waits in
`awaitOperatorAfterPark` for an operator `retry` of this ceiling's `lifetime-ceiling-reached` task
(`07 §3.3`). Its resolution `listen` correlates on `ceilingTaskRef` as well as the order, so it
consumes no other task's resolution: while that task is open, `07 §4.4` offers no `retry`,
`override` or `cancel` on any other task of the instance, and an operator resolves those after
the unpark, in the stage that waits on them (decision D-122). `resolve-manual-task` records the
retry against the request row the task route wrote under the operator's authorization; `unpark`
refuses the ceiling's subject unless that recorded retry exists, so no unrecorded signal can
release a ceiling park. The wait also carries the `PT5M` SLA tick every task waiter carries
(`07 §4.8` item 3): `resolve-manual-task` `sla-check`, scoped to `ceilingTaskRef` and under that
task's own `slaRound`, escalates the ceiling task to the Seller Operator once its stored
deadline has passed and never resolves it; it checks no other task, whose deadlines the waiter of
the stage that owns them re-checks after the unpark (decision D-129). The ceiling task offers no
task `cancel` (`07 §4.4`), because closing it would leave the order parked with nothing to wait
on: the Seller Operator ends a ceiling park with the order cancel of
[`09 §3.3`](./09-read-and-authz.md#33-api-contracts), which this wait's cancel arm consumes and
the fence unwinds (`parked → compensating`), or, before fulfillment has begun, where that route
refuses, with Lifecycle's own cancel, whose `OrderCancelled` the lifecycle arm consumes (D-109).
Every way from this wait into the unwind — that cancel, a supersession or a terminal order event —
passes `run-cancellation-fence`, whose step 1 closes the open `lifetime-ceiling-reached` task
through slice 07's closure port, so the unwind's compensation tasks are actionable and its
resolution `listen` cannot take the ceiling task's `retry` for a compensation retry (`06 §3.6`
`inst-fence-step1`). Each ceiling's task is its own row, keyed by its `ceiling:{round}` subject
(`07 §3.7`), so a second ceiling neither reopens the first one's task nor replays its SLA rounds
(decision D-149). `unpark` restores the pre-park phase and the process resumes at the stage
and checkpoint `ceilingEntry` recorded, under the fresh ceiling of the re-entered fork.
`ceilingEntry` also records the interrupted stage's `returnStage` as `ceilingReturnBack`, and
`backToProcess` restores it: the ceiling stage's own `leave` sets `returnStage` to `ceiling` so
that a cancel or lifecycle arm taken from its wait comes back to it, and without the restore a
lifecycle, hold, resume or cancel stage the ceiling interrupted would go back to the ceiling stage
and escalate a new ceiling round instead of returning where it came from (decision D-161). A
ceiling that fires while a lifecycle or cancel stage taken **from** the ceiling wait is running
(`returnStage = ceiling`, which only the ceiling stage's `leave` writes and `backToProcess`
restores) re-arms without parking, like the verdict park: the instance is already parked at the
open ceiling, whose task keeps its own SLA tick, and `ceilingEntry` is not run, so the first
ceiling's saved stage, checkpoint and return are kept (decision D-163). The
canonical version has **no `unpark-requested` arm**: `:plugin-control` is authorized
platform-side (`../ADR/0010`), so a signal with no Orders origin route is not a signal no one can
send, and the arm is removed until Q-13 gives it an origin route and a request row
([`09 §3.3`](./09-read-and-authz.md#33-api-contracts)). The terminal-event case
(`OrderCancelled`, `OrderExpired`, `OrderRejected`) is the same unwind entered through
`terminate-on-terminal-event` in fragment (f).

#### (e) Hold and resume

**ID**: `cpt-cf-bss-orders-workflow-seq-def-hold-resume`

**Use cases**: `cpt-cf-bss-orders-workflow-fr-owf-hold-resume`

**Actors**: `cpt-cf-bss-orders-workflow-actor-owf-orders-lifecycle`, `cpt-cf-bss-orders-workflow-actor-owf-generic-approval`

```yaml
# the hold arm of every stage fork except the ceiling wait and the unwind: it only listens
- awaitHold:
    listen:
      to:
        one:
          with: { type: gts.cf.core.events.event.v1~cf.bss.orders.event.v1~cf.bss.orders.held.v1~ }
          correlate:
            orderId:      { from: '${ .data.orderId }',      expect: '${ $context.orderId }' }
            orderVersion: { from: '${ .data.orderVersion }', expect: '${ $context.orderVersion }' }
      read: envelope
    output: { as: '${ .[0] | { eventId: .id } }' }
- arm: { set: { arm: hold, lifecycleEventId: '${ .eventId }', triggerKind: OrderHeld, preAdmitted: false } }   # every arm that writes lifecycleEventId clears preAdmitted: an admission answers only for its own event (D-161)
# the stage-level (early) resume arm beside it: a resume delivered before its hold is not lost (08 §4.7 item 3)
- awaitResume:
    listen:
      to:
        one:
          with: { type: gts.cf.core.events.event.v1~cf.bss.orders.event.v1~cf.bss.orders.resumed.v1~ }
          correlate:
            orderId:      { from: '${ .data.orderId }',      expect: '${ $context.orderId }' }
            orderVersion: { from: '${ .data.orderVersion }', expect: '${ $context.orderVersion }' }
      read: envelope
    output: { as: '${ .[0] | { eventId: .id } }' }
- arm: { set: { arm: resume, lifecycleEventId: '${ .eventId }', resumeEventId: '${ .eventId }', triggerKind: OrderResumed, preAdmitted: false } }   # suspensionRef is kept: the resume stage clears it only once a suspension is closed (D-141)
```

The hold stage, the `do` list of `process.hold`:

```yaml
- enter:
    switch:
      - waiting: { when: '${ $context.stageLoop == "awaitResume" }', then: awaitResume }
      - fresh:   { then: admitHold }
- admitHold:                            # protected (02): admission before consumption
    timeout: step
    try:
      - call: { step: admit-trigger }   # body: ref + triggerEventId: $context.lifecycleEventId, triggerKind: OrderHeld, role: listen
    catch: *transient
    export: { as: '${ $context + { admission: .admission, supersededByOrderVersion: (if .admission == "supersede" then .currentOrderVersion else null end) } }' }   # the version Lifecycle's read returned, the only source of the supersession audit (02 §4.3, D-155)
- onHoldAdmission:
    switch:
      - advance: { when: '${ $context.admission == "advance" }', then: applyHold }
      - onward:  { when: '${ $context.admission == "supersede" or $context.admission == "terminate" }', then: toLifecycle }
      - back:    { then: back }
- toLifecycle: { set: { nextStage: lifecycle, preAdmitted: true }, then: exit }
- applyHold:                            # protected (08): phase → suspended; pauses the gate windows through slice 03's gate-window port
    timeout: step
    try:
      - call: { step: apply-hold }      # body: ref + holdEventId: $context.lifecycleEventId, gateRefs: ($context.gateRefs // []); output: holdOutcome, suspensionRef
    catch: *transient
    export: { as: '${ $context + { holdOutcome: .holdOutcome, suspensionRef: .suspensionRef, resumePollRound: (if .suspensionRef != null and .suspensionRef == $context.suspensionRef then ($context.resumePollRound // 0) else 0 end), heldTicks: 0 } }' }   # the poll family is per suspension (D-133)
- onHold:
    switch:
      - pause:  { when: '${ $context.holdOutcome == "suspended" and $context.holdPauses }', then: enterResumeWait }   # only the approval stage waits on the resume
      - record: { then: back }          # suspended elsewhere, or reconciled-out-of-order | absorbed-duplicate | not-applicable: the stage continues, dispatch defers while suspended
- enterResumeWait: { set: { heldStage: '${ $context.returnStage }', heldLoop: '${ $context.stageLoop }', stageLoop: awaitResume } }   # resumePollRound as applyHold left it: the poll family is per suspension, reset only for a new suspensionRef, so a suspension already polled in another wait is not polled again from round 0 (D-130, D-133)
- awaitResume:
    fork:
      compete: true
      branches:
        - resume:    { do: [ ‹resume arm above› ] }   # 08 §4.7 item 4: resumeEventId exported from the listen
        - resumePoll: { do: [ { waitResumePoll: { wait: PT15M } }, { arm: { set: { arm: resumePoll } } } ] }   # stopgap for a resume lost between listens (D-124, D-130)
        - lifecycle: { do: [ ‹lifecycle arm of (f)› ] }
        - cancel:    { do: [ ‹cancel arm of (d)› ] }  # a denied cancel returns here: stageLoop is awaitResume
- afterResumeRace:
    switch:
      - poll: { when: '${ .arm == "resumePoll" }', then: pollResume }
      - any:  { then: leave }           # resume → the resume stage; lifecycle; cancel: an authorized cancel taken from hold, suspended → compensating
- pollResume:                           # protected (08): reads the order through Lifecycle; applies the resume only if Lifecycle no longer holds it, otherwise records nothing
    timeout: step
    try:
      - call: { step: apply-resume }    # body: ref + trigger: poll, suspensionRef, round: $context.resumePollRound; output: resumeOutcome ∈ resumed-by-read | still-held, due, failedTaskRefs[], nextRound
    catch: *transient
    export: { as: '${ $context + { resumeOutcome: .resumeOutcome, resumePollRound: .nextRound, resumeDue: (.due // false) } }' }
- onPollResume:                         # no failure route: the resume wait is entered only from the approval stage, before begin-fulfillment, so no failure is deferred (D-143)
    switch:
      - stillHeld: { when: '${ $context.resumeOutcome == "still-held" }', then: awaitResume }
      - resumed:   { then: backFromPoll }
- backFromPoll: { set: { nextStage: '${ $context.heldStage }', returnStage: '${ $context.heldStage }', stageLoop: '${ $context.heldLoop }', suspensionRef: null }, then: exit }   # the suspension is closed: no later wait polls it (D-133)   # the stage and loop the hold interrupted; resumeDue is the escalation re-check's first answer
- leave: { set: { nextStage: '${ .arm }', returnStage: hold }, then: exit }
- back: { set: { nextStage: '${ $context.returnStage }' }, then: exit }
```

The resume stage, the `do` list of `process.resume` — entered from the resume wait
(`returnStage = hold`, `suspensionRef` set) or from a stage-level resume arm (`suspensionRef` as
the stage holds it; the call passes null and the operation resolves the order's open row):

```yaml
- enter:
    switch:
      - fromWait: { when: '${ $context.returnStage == "hold" }', then: admitResume }   # heldStage and heldLoop recorded by the hold stage
      - early:    { then: noteEarly }
- noteEarly: { set: { heldStage: '${ $context.returnStage }', heldLoop: '${ $context.stageLoop }' } }
- admitResume:                          # protected (02)
    timeout: step
    try:
      - call: { step: admit-trigger }   # body: ref + triggerEventId: $context.resumeEventId, triggerKind: OrderResumed, role: listen
    catch: *transient
    export: { as: '${ $context + { admission: .admission, supersededByOrderVersion: (if .admission == "supersede" then .currentOrderVersion else null end) } }' }   # the version Lifecycle's read returned, the only source of the supersession audit (02 §4.3, D-155)
- onResumeAdmission:
    switch:
      - advance: { when: '${ $context.admission == "advance" }', then: applyResume }
      - onward:  { when: '${ $context.admission == "supersede" or $context.admission == "terminate" }', then: toLifecycle }
      - back:    { then: back }
- toLifecycle: { set: { nextStage: lifecycle, preAdmitted: true }, then: exit }
- applyResume:                          # protected (08): phase → started; re-arms the gate windows through the gate-window port
    timeout: step
    try:
      - call: { step: apply-resume }    # body: ref + resumeEventId, suspensionRef: '${ if $context.returnStage == "hold" then $context.suspensionRef else null end }' — the recorded ref from the resume wait, null on the stage-level arm (08 §4.7 item 3); the operation reads the order first (08 inst-ar-poll, D-141); output: resumeOutcome, due, failedTaskRefs[] (lineRef + reason)
    catch: *transient
    export: { as: '${ $context + { resumeOutcome: .resumeOutcome, resumeDue: (.due // false), resumeFailed: (.failedTaskRefs // []) } }' }   # no routing member is written from the answer (rule 1)
- onResume:                             # still-held first; then no failure route before begin-fulfillment (D-143); then 08 §4.7 item 8: deferred failures first, before any dispatch
    switch:
      - stillHeld:        { when: '${ $context.resumeOutcome == "still-held" }', then: onStillHeld }   # Lifecycle holds the order again: nothing was closed (D-141)
      - preFulfillment:   { when: '${ $context.heldStage == "approval" or $context.heldLoop == "awaitEligibilityChange" }', then: backResumed }   # nothing is deferred before begin-fulfillment
      - deferredFailures: { when: '${ ($context.resumeFailed | length) > 0 }', then: resumeFailure }
      - resumed:          { then: backResumed }
- onStillHeld:
    switch:
      - fromWait: { when: '${ $context.returnStage == "hold" }', then: backToResumeWait }
      - early:    { then: stillHeldBack }
- backToResumeWait: { set: { nextStage: hold, returnStage: hold, stageLoop: awaitResume }, then: exit }   # suspensionRef and resumePollRound kept: the wait polls the same suspension
- stillHeldBack: { set: { nextStage: '${ $context.heldStage }', returnStage: '${ $context.heldStage }', stageLoop: '${ $context.heldLoop }' }, then: exit }   # suspensionRef kept, so a polled wait keeps polling it (D-133)
- backResumed: { set: { nextStage: '${ $context.heldStage }', returnStage: '${ $context.heldStage }', stageLoop: '${ $context.heldLoop }', suspensionRef: null }, then: exit }   # the stage and loop the hold interrupted; apply-resume's due is the escalation re-check's first answer
- resumeFailure:                        # fragment (c)
    set: { failureScope: line, failureSubjects: '${ [ $context.resumeFailed[] | { subjectRef: .lineRef, reason: .reason, cause: (if .reason == "intent-unresolved" then "sweep-floor-reached" else "sweep-discovered-terminal-failure" end) } ] }', sourceStep: apply-resume, sourceAttempt: '${ $context.resumeEventId }', nextStage: failure, stageLoop: null, suspensionRef: null }
    then: exit
- back: { set: { nextStage: '${ $context.returnStage }' }, then: exit }
```

**Description**: The fragment reproduces [`08 §3.6` *Hold Then Resume with Remaining-Window
Timer Preservation*](./08-hold-and-cancel.md#36-interactions--sequences) as a definition pattern,
in the order of [`08 §4.7`](./08-hold-and-cancel.md#47-constraints-this-slice-places-on-the-definition).
**Only the approval-escalation window pauses on hold**: the hold arm sits inside `gateLoop` (and
`outageArm`), and because it only listens, the hold winning the race cancels the
`waitProbe` tick at once. The hold stage admits the event, and `apply-hold` records the
suspension and pauses the gate windows through slice 03's gate-window port; in the approval stage
(`holdPauses`) the definition then waits in `awaitResume`, and after `apply-resume` the approval
stage is re-entered at `gateLoop`. The remainder is never the definition's: slice 03's
gate-window port stores it in `owf_approval_gate.window_remaining_ms` and re-bases the gate's
escalation deadline on resume; `apply-resume` answers `due` against that deadline, and the
approval stage's `enter` switch takes a `due: true` straight to `escalateGate` before the next
tick. `apply-resume` does not re-read drafts: the draft-liveness re-read is inside the wave-2
dispatch (slice 05). A non-empty `failedTaskRefs[]` (failures slice 05 recorded as deferred while
held) routes to fragment (c) before any dispatch. **The lifetime ceiling, the barrier and the
overdue window keep running**: the ceiling and the overdue monitor are top-level branches of
fragment (a), outside every stage; outside the approval stage a hold is recorded and the stage
loop continues, so the barrier poll and the `waitExpected` tick are never cancelled by a hold, and
the dispatch operations defer while `owf_process_instance.suspended` is set
([`01 §3.7`](./01-foundation.md#table-owf_process_instance)); the stage-level resume arm then
records the resume. Every stage fork that carries a hold arm also carries that resume arm, so a
resume delivered before its hold is recorded as `resume-ahead-recorded` and not lost, and the
resume wait is entered only on `holdOutcome = suspended`. **The resume wait polls the hold**
(decision D-130), a stopgap until the event-retention ask of §4.4 lands: its `PT15M`
`waitResumePoll` branch calls `apply-resume` with `trigger: poll`, which reads the order through
the Lifecycle PDP-authorized order read. While Lifecycle holds the order it records nothing and
answers `still-held` with the next round, and the wait continues; once Lifecycle no longer holds
it, it applies the resume as for an `OrderResumed` — closes the suspension, re-arms the gate
windows, applies the deferred failures — and answers `resumed-by-read`, and the definition
returns to the stage and loop the hold interrupted. A resume lost while the hold stage ran
`admitHold` and `applyHold` therefore costs at most one poll interval, not the lifetime ceiling.
**Every other wait polls a recorded suspension too** (decision D-133). Outside the resume wait a
hold is only recorded: in the park loop, in the fulfillment waits and in a wave deferral answered
`held`. There, a resume lost between listens would leave `owf_process_instance.suspended` set, the
dispatch operations deferring, and a held deferral looping until the ceiling, since a lost event
reaches no `listen` (each deferral wait carries the four shared arms, D-142). So while `$context.suspensionRef` is set, each such wait of fragments (a) and (b) counts
its tick in `heldTicks` and calls `pollHeld` about every `PT15M`. The way out of the park loop
polls once more, so a suspension recorded while parked is not carried into `gateLoop`. `gateLoop`
and `outageArm` do not poll: a hold there enters the resume wait, and their tick carries the
escalation fire's lateness bound (D-123). That is the same
`apply-resume` `trigger: poll` under the same per-suspension `resumePollRound`, which `applyHold`
resets whenever it answers a new `suspensionRef`. A `resumed-by-read` answer clears
`suspensionRef`, as `backFromPoll` does after the resume wait's poll, so no closed suspension is
polled again. The resume stage clears it too, once `apply-resume` has closed a suspension; a `still-held` answer keeps it. A later `OrderResumed` for a suspension the
poll closed is attached to it as an absorbed duplicate (`08` `inst-ar-ahead`), wherever it
arrives. The failure stage's resolution wait does not poll, because an operator resolves it and
every way out of it reaches a polled wait or the unwind.
**A resume Lifecycle has already overtaken closes nothing** (decision D-141). Neither
`OrderHeld` nor `OrderResumed` names the hold it belongs to, so `apply-resume` reads the order
before it acts on an event too. If Lifecycle holds the order `on_hold` again, it answers
`still-held`. The resume stage then returns to the resume wait, with `suspensionRef` kept and the
poll still running, or to the stage the arm interrupted. The suspension stays open for the later
hold. `apply-hold` applies the same read to a `resume_ahead` row: it consumes the row, and it
still records the suspension when Lifecycle holds the order. **No failure route before
begin-fulfillment** (decision D-143). The resume wait is entered only from the approval stage,
where no dispatch has run and nothing is deferred, so its poll has no failure route. The resume
stage takes its failure route only when the interrupted stage is past the eligibility wait. The
poll consumes no Lifecycle event, so no `admit-trigger` precedes it; the order read inside
`apply-resume` is its guard (§4.1). A cancel taken from the resume wait
that `authorize-cancel` denies returns to the resume wait, because the instance is still
`suspended`. The ceiling wait and the unwind carry no hold arm (`06 §4.7` item 7); the park loop's
hold and resume arms only record, because `holdPauses` is false there and the park clock is never
paused (`03 §4.5` item 6), and the held waits of (a) and (b) carry both so that a held reflection,
spawn signal or completion report is called again after the resume (`01 §3.3` *Rounds and attempts*). With the re-check loop the hold pattern needs no Function (§4.5).

#### (f) Amendment and terminal order events

**ID**: `cpt-cf-bss-orders-workflow-seq-def-amendment-and-terminal`

**Use cases**: `cpt-cf-bss-orders-workflow-fr-owf-start-contract`, `cpt-cf-bss-orders-workflow-fr-owf-terminal-order-events`, `cpt-cf-bss-orders-workflow-fr-owf-approval-request`

**Actors**: `cpt-cf-bss-orders-workflow-actor-owf-orders-lifecycle`

```yaml
# the lifecycle arm of every stage fork: it only listens
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
      read: envelope
    output:                             # triggerKind by exact-type lookup into admit-trigger's closed nine-value enum (02 §3.3), as input.from does for the start (D-107); nothing branches on it
      as: >-
        ${ .[0] | { eventId: .id,
                    triggerKind: ({ "gts.cf.core.events.event.v1~cf.bss.orders.event.v1~cf.bss.orders.amended.v1~": "OrderAmended",
                                    "gts.cf.core.events.event.v1~cf.bss.orders.event.v1~cf.bss.orders.cancelled.v1~": "OrderCancelled",
                                    "gts.cf.core.events.event.v1~cf.bss.orders.event.v1~cf.bss.orders.expired.v1~": "OrderExpired",
                                    "gts.cf.core.events.event.v1~cf.bss.orders.event.v1~cf.bss.orders.rejected.v1~": "OrderRejected" }[.type]) } }
- arm: { set: { arm: lifecycle, lifecycleEventId: '${ .eventId }', triggerKind: '${ .triggerKind }', preAdmitted: false } }   # preAdmitted false: a new event is admitted by the stage, never routed on an earlier event's admission (D-161); no version is copied from the event: an OrderCancelled at the pinned version carries the instance's own (D-155)
```

The lifecycle stage, the `do` list of `process.lifecycle` — admission first, and the admission
alone decides (`02 §4.7` items 1 and 3):

```yaml
- enter:
    switch:
      - preAdmitted: { when: '${ $context.preAdmitted == true }', then: onLifecycleAdmission }   # the hold, resume or acceptance admission already answered supersede | terminate
      - fresh:       { then: admitLifecycle }
- admitLifecycle:                       # protected (02)
    timeout: step
    try:
      - call: { step: admit-trigger }   # body: ref + triggerEventId: $context.lifecycleEventId, triggerKind, role: listen; output: admission ∈ advance | supersede | terminate | absorbed-duplicate | ignored-superseded | ignored-terminated
    catch: *transient
    export: { as: '${ $context + { admission: .admission, supersededByOrderVersion: (if .admission == "supersede" then .currentOrderVersion else null end) } }' }   # the version Lifecycle's read returned, the only source of the supersession audit (02 §4.3, D-155)
- onLifecycleAdmission:
    switch:
      - supersede: { when: '${ $context.admission == "supersede" }', then: supersedePath }
      - terminate: { when: '${ $context.admission == "terminate" }', then: terminalEvent }
      - back:      { then: back }       # absorbed-duplicate (an OrderAmended at the pinned version) | ignored-superseded | ignored-terminated
- supersedePath:                        # fragment (c): fence (supersede) cancels open gates, voids un-activated wave-1 drafts, compensates activated; report-outcome makes no seam call
    set: { unwind: supersede, triggerEventId: '${ $context.lifecycleEventId }', preAdmitted: false, nextStage: unwind, stageLoop: null }
    then: exit
- terminalEvent:                        # protected (02): OrderCancelled | OrderExpired | OrderRejected for an active instance
    timeout: step
    try:
      - call: { step: terminate-on-terminal-event }   # body: ref + triggerEventId: $context.lifecycleEventId; output: terminate (bool)
    catch: *transient
    export: { as: '${ $context + { terminate: .terminate } }' }
- onTerminal:
    switch:
      - unwind: { when: '${ $context.terminate }', then: terminalUnwind }
      - back:   { then: back }          # terminate: false returns to the arm's stage and never skips to terminate-instance (02 §4.7 item 4)
- terminalUnwind:                       # report-outcome makes no Lifecycle transition: the order is already terminal
    set: { unwind: terminal-event, triggerEventId: '${ $context.lifecycleEventId }', preAdmitted: false, nextStage: unwind, stageLoop: null }
    then: exit
- back: { set: { nextStage: '${ $context.returnStage }', preAdmitted: false }, then: exit }
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
operation, and the returned `admission`, never event data, selects the path; a hold, resume or
acceptance admission that answers `supersede` or `terminate` enters this stage with
`preAdmitted` and is routed without a second admission. Every arm that writes `lifecycleEventId`
— acceptance, hold, resume and lifecycle — sets `preAdmitted: false`, so `preAdmitted` is true
only while `lifecycleEventId` is the event whose admission set it: a ceiling that interrupts this
stage, followed by a lifecycle arm in the ceiling wait, sends the new event through
`admitLifecycle` rather than routing it on the earlier event's answer. Nothing is lost by the
overwrite, because `supersede` and `terminate` are read from Lifecycle's order state, which never
leaves superseded or terminal, so the new event's admission answers `supersede` or `terminate`
whenever the earlier one did (`02 §3.3` *Trigger-to-outcome table*, decision D-161). Termination is symmetric with start: a
terminal order event and a superseding version both run the cancellation fence and the
compensation walk before `terminate-instance`, and neither leaves a wave-1 draft for a platform
TTL this gear does not own. The admission decision — is this event for a superseded, current or
newer version — stays in `admit-trigger`, read against Lifecycle under R1, never in a jq
comparison over event data.

### 3.7 Database schemas & tables

This document owns **no table**. Definition versions live in the platform function registry
(`functions`, [serverless-runtime DESIGN `DESIGN.md:1143`](../../../../serverless-runtime/docs/DESIGN.md#logical-tables));
the binding of an instance to a version is `owf_definition_binding`
([`01 §3.7`](./01-foundation.md#table-owf_definition_binding)); the operations a definition may
call are `owf_step_operation` ([`01 §3.7`](./01-foundation.md#table-owf_step_operation)). The
complete canonical definition is kept **in this repository** as `definitions/order-process.yaml`
under the gear; it is the CI artefact that validates against the Serverless Workflow DSL 1.0.0
schema and the rules of §2.2, and the publish job of §4.2 publishes it after writing the
environment's step base URL into `input.from`, asserting that nothing else differs. The fragments
of §3.6 are an abridged review view of that file, not its bytes.

### 3.8 Deployment Topology

- [ ] `p3` - **ID**: `cpt-cf-bss-orders-workflow-topology-process-definition`

The definition executes on the platform's Temporal plugin workers
([serverless-runtime DESIGN §1.4.4](../../../../serverless-runtime/docs/DESIGN.md#144-gear-lifecycle)),
which are a platform deployable, not an Orders one; Orders deploys no workflow worker. Orders'
side of the topology is: the step surface of `01 §3.3` reachable from the plugin's workers under
the service principal; the two event triggers of §3.3 (`execution_context: system`)
provisioned once per environment and enabled only after the readiness gate; the validation hook of §3.2 registered with the platform registry once
the platform calls a consumer hook (a pending ask; until then the CI test is the gate); and the repository
`definitions/` directory published to the registry by the publish job of §4.2, never by hand. The two
trigger bindings are part of that directory (decision D-107): their event type, filter,
`callable_type` and `execution_context` are reviewed and CI-checked with the definition — the CI
test asserts exactly the two bindings of `02 §2.2`, both `execution_context: system`, the
`OrderSubmitted` one filtered to `new_sale` — and applied by the same pipeline under the same
platform-operator publish role, and the readiness check compares the bindings the platform lists
for `order_process` with the repository's and reports any drift. A binding edited by hand is
therefore a drift alert, and the run-time guard behind it is `admit-trigger`'s re-check. One
definition version is **active for new instances** per environment at a time: the publish job
publishes a version and deprecates the one it replaces in the same run, and rolls back by
publishing the last good document, with its `document.version` set to the new version, as a new
version (§4.2, decisions D-138, D-158). Older versions stay
published — `deprecated`, still callable by the instances pinned to them — and are never archived
or deleted by the job.

**Observability owned here**: active definition version per environment; count of instances
bound per version (`01 §3.8`); new bindings in the last hour to any version other than the active
one, and bound versions the registry no longer lists as `active` or `deprecated` (§4.2); validation-hook refusals by rule; signal deliveries by type and
delivery outcome; the platform's invocation status distribution for `order_process` read from
`GET /api/serverless-runtime/v1/invocations` (`DESIGN.md:866`) — `suspended` is the normal state
of a healthy long-running instance; `failed`, `dead_lettered`, `canceled`, `compensating`,
`compensated` and `succeeded` on a bound non-terminal instance are what the `reconciliation-sweep`
worker's instance liveness pass raises as an `invocation-dead` task (`01 §3.8`, D-105).

## 4. Definition Normative Rules

### 4.1 The fence

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-constraint-definition-fence`

Every published version **MUST** contain the protected operations of
[`01 §3.3`](./01-foundation.md#the-step-operation-contract) and the slices' §3.3 declarations in
the following order constraints, and the validation hook **MUST** refuse a version that violates
any of them:

| Stage | Protected operations, in order | May be interleaved with (composable) |
|-------|-------------------------------|--------------------------------------|
| Admission | `admit-trigger` (`role: start`) **<** `start-instance`; on every arm that consumes a Lifecycle trigger, `admit-trigger` (`role: listen`) **<** the consuming operation (`evaluate-payment-auth-eligibility`, `apply-hold`, `apply-resume`, `terminate-on-terminal-event`, `run-cancellation-fence` on supersede); `apply-resume` with `trigger: poll` consumes no trigger and follows no admission — the Lifecycle order read inside it is its guard (D-130), wherever it is called from (the resume wait, or `pollHeld` in another wait, D-133) | — |
| Verdict | `obtain-verdict` **<** `reflect-verdict`; `record-decision` **<** `reflect-verdict` on the decision path. The `p1` composables on their paths (decision D-135): on every walk where `verdict = unobtainable`, `park` **<** a park loop whose tick calls `arm-park-escalation`, with a walk from it to `raise-overdue-escalation` (`escalationKind: park`), and no `reflect-verdict`, `open-gates` or `nextStage: fulfillment` until a later `obtain-verdict` answers `required` or `not-required` (ADR-0007, `03 §4.5` items 2 and 6); on every walk where `reflected = pending_approval`, `open-gates` **<** `record-decision`, and the wait between them is a competing `fork` carrying the decision `listen` and a tick branch whose route calls `escalate-gate` `mode: fire` and then `mode: probe`, with a walk from a probe to `raise-overdue-escalation` (`escalationKind: approval-outage`) (`fr-owf-approval-request`, `fr-owf-approval-escalation`, `03 §4.5` items 1, 4 and 5) | `open-gates`, `escalate-gate`, `arm-park-escalation`, `park`, `unpark` (beyond those paths) |
| Plan | `evaluate-payment-auth-eligibility` **<** `construct-and-freeze-plan` **<** `begin-fulfillment` | `evaluate-activation-eligibility` |
| Waves | `begin-fulfillment` **<** `dispatch-wave1-create` **<** `re-check-pre-activation` **<** `report-spawn-signal` **<** `dispatch-wave2-activate`, and every walk that reaches either dispatch operation carries the pinned `beginResult = in-fulfillment` (decision D-143): the failure stage's return to the barrier and the resume stage's failure route are decided on it or on the interrupted stage, never on a returned list; every walk to `dispatch-wave2-activate` passes an `evaluate-activation-eligibility` answer whose pinned `released` is `true`, and the pinned `spawned = true` of a sent spawn signal, before it — the ADR-0004 conjunction: `released` is true only once database time has reached `expected_fulfillment_at` and every create is confirmed (`04 §3.6` `inst-ae-if-conjunction-false`), so the `waitExpected` re-check loop is where a walk waits while `due` is false, not a separate order constraint, and the run-time guard is `05 §3.6` `inst-pi-wave2-guard` (`activation-precondition-unmet`) (decision D-144); `evaluate-activation-eligibility` stays `composable` everywhere else | `evaluate-activation-eligibility` (beyond the conjunction), `reconcile-intent`, `reread-draft-liveness`, `rebuild-wave1` |
| Failure | `create-manual-task` before any terminal outcome on every walk where `policy = remediate` or `forceTask` is set; on a walk where `policy = fail-fast` and `forceTask` is not set, the failure route reaches the unwind with no `create-manual-task` (`07 §4.8` item 1), so the switch that picks the route reads the seller's pinned `policy` and no literal of its own (decision D-135). After every `create-manual-task`, and after the ceiling's `raise-overdue-escalation` (`lifetime-ceiling`), the wait that holds the task is a competing `fork` carrying the `task-resolution-requested` `listen`, whose route calls `resolve-manual-task` (`trigger: request`), and a tick branch whose route calls `resolve-manual-task` (`trigger: sla-check`) (`fr-owf-manual-task`, `nfr-owf-manual-task-sla`, `07 §4.8` item 3); `verify-override` only after a `resolve-manual-task` (`07 §4.8` item 5) | `resolve-manual-task` (which runs `retry-step` in-process), `verify-override`, `raise-overdue-escalation` (beyond those paths) |
| Unwind | `run-cancellation-fence` **<** `compensate-order` **<** `report-outcome` on every failure, cancel, supersede and terminal-event path; `authorize-cancel` **<** `run-cancellation-fence` on the cancel path; `terminate-on-terminal-event` **<** `run-cancellation-fence` on the terminal-event path | — |
| Hold | `apply-hold` **<** `apply-resume`, in the arm that owns the escalation `wait` | — |
| Termination | `terminate-instance` last on every path; `report-outcome` **<** `terminate-instance` where an outcome is reported | — |

`settle-from-lookup` and `retry-step` **MUST NOT** appear (D-108). No path **MAY** reach `report-outcome` with
`outcome: completed` except through `dispatch-wave2-activate`, and none **MAY** reach it with
`failed`, `cancelled` or `superseded` except through `compensate-order`. **Rationale**: these are
the steps the PRD's acceptance criteria and the seam rules R1–R5 make non-negotiable, and the
`p1` composables on their paths are the ones without which a `p1` requirement has no realisation
at all — the operations stay `composable`, so the counts of ADR-0012 (22 `protected`, 13
`composable`) do not change; everything else is the flow's to arrange.

### 4.2 The validation hook contract and the publish job

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-constraint-definition-validation-hook`

The eight rules of §2.2 *Validation before publish* **MUST** run as a CI test over the
repository copy of every canonical definition on every change to `definitions/` or to any slice's
§3.3 declaration, and a version **MUST NOT** be published except by the publish job below
(decision D-138).

**The publish job.** Definition publishing is a pipeline job of its own. It runs on a merge to
`definitions/` and **MUST NOT** build, package or deploy the gear, so a definition change reaches
an environment without an Orders release. A merge to `definitions/` needs the approval of the
repository's code owners for that directory, who are the Orders definition owners, and of a second
reviewer. A change that also touches an operation — its code, its slice §3.3 declaration or the
protected list — is an Orders release: it goes through the release pipeline, and the definition
that uses it is published once that release is deployed. In each environment, in promotion
order, the job:

1. runs the rules of §2.2 over the candidate against the `owf_step_operation` declarations of the
   Orders release deployed in that environment and against its step-surface base, and refuses a
   candidate whose `wave1` timeout is not below the smallest overdue window the environment's
   `owf_seller_policy` rows hold, which is the smallest effective overdue window of any seller
   (the policy side of the bound, [`01 §4.2`](./01-foundation.md#42-five-distinct-bounds-two-owners),
   decision D-159); it also compares every literal tick of the candidate with the value the
   guidance item of §4.7 fixes for it (the fixed-waits table of §3.6), and refuses a difference
   unless the same change amends that slice item (decision D-158);
2. in the first non-production environment only, runs the **behavioural gate**: it publishes the
   candidate there, deprecates the version it replaces and applies the two trigger bindings, all
   as step 3 does — so the suite's trigger-started orders reach the candidate, including a major
   bump's new callable id, and never an old major left with no `active` version — and drives a scenario suite with one order per path of §3.6 — approval
   not required, required and unobtainable (a); fulfillment through both waves (b); a partial
   failure under each policy (c); a cancel (d); a hold and resume (e); an amendment and each
   terminal event (f) — against that environment's real step surface, with test doubles for
   Lifecycle, Subscriptions and the approval service. After every step operation the suite asserts
   Orders' record, and at every route it asserts the slice items §4.7 lists as canonical-definition
   guidance. A business window is a seller-policy value (decision D-134), so the suite seeds short
   windows for its test seller; a tick longer than the suite's budget, and the `P90D` ceiling, are
   not exercised, and the suite reports them so: their values are held by the static comparison
   of step 1 and by rules 4 and 7. A failing scenario stops the job before any other environment
   is touched, and the job rolls that environment back forward, as *Rollback* below does, bindings
   included, so its triggers start no further order on the failed candidate; the suite's own orders still running
   on it are ended with the order cancel (decision D-158);
3. publishes the version, deprecates the version it replaces in the same run, and applies the two
   trigger bindings of §3.8; in the first non-production environment, where step 2 has already
   done all three, it does nothing more;
4. never archives or deletes a version.

**Which version starts new orders.** One version per callable id is `active` in an environment
after each run. The platform does not state which version an event trigger starts — the newest
`active` version of the major its `function_id` names, or a version pinned in the binding — and
offers no tenant-scoped or percentage activation; both are the ask
`cpt-cf-bss-orders-workflow-upreq-serverless-runtime-trigger-version-selection`
([`../UPSTREAM_REQS.md`](../UPSTREAM_REQS.md) §2.9). Until it lands the behavioural gate is the
only execution of a candidate before production, and the gear's readiness check alerts on a new
binding to any version other than the one the job left `active` (§3.8).

**Rollback** is forward. The job rolls a bad version back by publishing the last good
document **with its `document.version` set to the new minor** as a new minor version, which
deprecates the bad one; the document is otherwise unchanged. The rollback version is the next
minor above the highest version the first non-production environment's registry lists — every
candidate is published there first, so it holds the highest, the failed gate's included — and
the job commits the re-versioned document to `definitions/` in the same run, its only commit: a
copy of a reviewed, gated document whose `document.version` alone changed. The rollback is then
published through the normal job in promotion order, without the behavioural gate its content has
passed, so every environment holds the same content under each semver; an environment that never
held the failed candidate only skips that number. CI refuses a candidate whose `document.version`
is not above every version in `definitions/`, so the next fix is versioned above the rollback and
cannot collide with it (decision D-158 as amended). For a major bump the rollback is a new minor of
the old major, and the job re-points both bindings back to it. Its `version` must be the new one,
because `start-instance` refuses `definition-not-bound` when the document's own `version` differs
from the version the platform pinned (§4.3, decision D-137), so a document republished under its
old `version` would stop every new order from starting. The registry does allow a two-step return
to an older version, `deprecated → disabled → active`
([serverless-runtime DESIGN](../../../../serverless-runtime/docs/DESIGN.md#function-status-state-machine)
lines 588–607), and the design still prefers the forward version: that path passes the good
version through `disabled` while instances pinned to it are live, and the platform does not say
what disabling a version does to its running invocations, while a forward minor changes no
version that has live instances except to deprecate the bad one (decision D-158). Instances
already pinned to the bad version run on it to termination (§4.3); an operator ends one that
misbehaves with the order cancel or through its manual tasks.

**A major bump** changes the callable id (`…order_process.v<major>~`, §3.1). The job publishes the
new major, re-points both trigger bindings to it and deprecates the old major's `active` version in
the same run; instances of the old major run to termination on it.

**Bound versions** are checked in each environment, not by the repository's CI, which cannot read
an environment's `owf_definition_binding`. The gear's readiness check compares the versions its
bindings name with the registry's version listing and alerts on one that is neither `active` nor
`deprecated`; because the job never archives or deletes, such a version is the trace of a publish
or transition outside the job.

Whether the platform registry calls a consumer-supplied hook before
publish is a **pending upstream ask**
(`cpt-cf-bss-orders-workflow-upreq-serverless-runtime-definition-versioning-validation-hook`,
[`../UPSTREAM_REQS.md`](../UPSTREAM_REQS.md) §2.9); the platform today has only the plugin's
registration-validation hook (`DESIGN.md:762`). When the ask lands, the validation hook of §3.2
**MUST** run the same rules over every candidate version before publish, **MUST** name the rule
and the task location in the platform's `ValidationError` shape, and **MUST** refuse the
platform's `archive` and `delete` transitions for a version an `owf_definition_binding` names;
until then a publish outside the job is an unvalidated publish this design does not permit, which
the same ask asks the platform to refuse by restricting publishes of `order_process` to the job's
identity (decision D-137).

### 4.3 Pinning

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-constraint-definition-pinning`

An instance **MUST** run to termination on the version `start-instance` bound
([`01 §3.7`](./01-foundation.md#table-owf_definition_binding)); no operation **MAY** rewrite the
binding, no operator surface **MAY** migrate an instance (PRD §5.2), and a new version **MUST**
affect only instances started after its publication. The platform's own pin of the invocation to
the callable version ([`DESIGN.md:614`](../../../../serverless-runtime/docs/DESIGN.md#versioning-model))
and Orders' binding **MUST** agree: `start-instance` records the `function_id` and
`function_version` that the platform's invocation record reports for the bound invocation
(`GET /api/serverless-runtime/v1/invocations/{invocation_id}`,
[DESIGN_GTS_SCHEMAS.md](../../../../serverless-runtime/docs/DESIGN_GTS_SCHEMAS.md#invocationrecord)),
never the document's self-declared `version`, which it only compares. A document whose `version`
differs from the registry semver the platform pinned, or an invocation of another callable, is
refused `definition-not-bound` (decision D-137). A new binding to a version other than the one the
publish job left `active` is an alert (§4.2), not a silent choice.

### 4.4 Signal semantics

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-constraint-definition-signals`

Hold and resume **MUST** be consumed as the Lifecycle events `OrderHeld` and `OrderResumed`
through `listen`, never as platform `suspend`/`resume` control actions, because a platform
suspension would pause every `wait` including the lifetime ceiling. An operator cancel **MUST** be
delivered as the `cancel-requested` signal to the running invocation and **MUST NOT** use the
platform's generic `cancel`, which ends the invocation without the fence. A re-authorisation
**MUST** be delivered as `reauthorize-requested`. Every signal **MUST** be recorded in Orders
before delivery — the request row the originating control operation of `09` writes; slice 10
delivers that row and records none — and **MUST** be delivered idempotently under the
caller-side duplicate protocol. For `reauthorize-requested` and `unpark-requested` the
record-before-delivery rule is **deferred until Q-13** is answered: neither has an origin route
or a request row in this design set (`09 §3.3`), so neither is delivered today; a signal with no consuming arm in
the current stage **MUST** be held by the plugin's subscription until an arm consumes it, and if
the platform cannot guarantee that, the originating control operation **MUST** answer
`still-processing` until Orders observes the arm's recording operation (`authorize-cancel`,
`evaluate-payment-auth-eligibility`, `resolve-manual-task`). The exact `:plugin-control` verb and payload are an
upstream ask (§3.3); this rule binds regardless of their shape.

**Events delivered between listens.** A broker event — `OrderHeld`, `OrderResumed`, the approval
decision, `OrderAmended`, a terminal order event, a Subscriptions outcome — can reach the
invocation while no `listen` for it is armed: during every step call a stage makes, and between a
competing fork's teardown and its re-arm. The canonical definition **MUST** be run only on a
platform that retains such an event for the invocation and delivers it, in order, to the next
`listen` that matches it (decision D-124, ask
`cpt-cf-bss-orders-workflow-upreq-serverless-runtime-event-retention-between-listens`). One poll
covers one loss, as a stopgap (decision D-130): the resume wait's `PT15M` `waitResumePoll` reads
through `apply-resume` whether Lifecycle still holds the order, so a resume delivered while the
hold stage runs `admitHold` and `applyHold` is applied at most one poll interval late. Nothing
re-reads a hold, a decision or an amendment. The stage-level resume arm covers only a resume
delivered before its hold's `listen` (`08 §4.7` item 3), and the barrier and eligibility polls
cover only the conditions they evaluate. A resume lost outside the approval stage's resume wait
is covered by the same poll: every other wait that holds a recorded suspension calls `pollHeld`
about every `PT15M` (decision D-133). It is applied at most one poll interval late (one `PT1H`
tick in `awaitExpected`), not held until an operator cancels the order or the lifetime ceiling
parks it. A decision delivered during a probe is still not recorded, so the gate escalates on its
window.

Orders issues no generic `cancel`, `suspend` or `resume`, and it **cannot prevent** another
platform-authorized caller from issuing them (`../ADR/0010` *platform APIs are authorized
platform-side*). What it does instead (decision D-105): a `canceled` invocation, and a suspension
that times out into `failed`, are raised by the instance liveness pass as an `invocation-dead`
task (`01 §3.8`); the platform is asked to deny generic `cancel`, `suspend` and `resume` on
`order_process` invocations to every caller and `retry` to all but this gear's control gateway
(`…-upreq-serverless-runtime-invocation-control-restriction`). Until then a generic `suspend`
freezes the order, including the lifetime ceiling, undetected until the platform's suspension
limit fails it.

### 4.5 The hold pattern, and Q-11

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-constraint-definition-hold-pattern`

The approval-escalation re-check tick **MUST** be inside a competing `fork` that also contains
the hold arm, so that a hold cancels it; the window itself **MUST** stay Orders' record —
`apply-hold` pauses it and `apply-resume` re-bases it through slice 03's gate-window port
(`owf_approval_gate.window_remaining_ms`) — and the definition **MUST NOT** carry or re-arm a
remainder: it re-checks the stored deadline through `escalate-gate` `mode: fire` and takes
`apply-resume`'s `due` as the first answer after a resume. The lifetime `wait` **MUST** be a
literal `P90D` in a top-level competing branch outside every stage, and the barrier poll, the
expected-fulfillment re-check and the overdue monitor **MUST** be in branches a hold arm does not
cancel: outside the approval stage a hold is recorded and the stage loop continues (§3.6 (e)).
Every computed deadline **MUST** be a bounded re-check loop of §3.6 *Fixed waits and re-check
loops*; a Function **MUST NOT** be used to sleep, because serverless-runtime Functions are bounded
by platform timeout limits and durable waits belong to Workflows (`DESIGN.md:579`,
`DESIGN.md:582`). **Q-11 is registered open** (`../DECISIONS.md`): whether the Serverless
Workflow DSL 1.0.0, as the platform's plugin implements it, (i) accepts a runtime-expression
`wait` duration **as an extension** — the 1.0.0 schema admits only an inline duration or an ISO
8601 string, so this is a question about the plugin, not about the specification; (ii) surfaces
the Problem body's `error_code` on `$error` so a `catch` can tell `idempotency-key-conflict` from
`still-processing` before the retry budget is spent; (iii) offers any dynamic parallel construct,
so a wave could fan out per line inside the definition rather than inside the operation; (iv)
treats a `listen` inside a competing `fork` as cancellable without losing an event delivered
during cancellation; (v) expresses the hold pattern natively; and (vi) re-raises the last error
when a `catch` that carries only `retry` spends its limit, which §4.6 and rule 6 of §2.2 take as
the fault of the invocation — the DSL says nothing about what follows the last attempt (dsl.md
*Retries*, dsl-reference.md *Catch*). Until Q-11 is answered, (i) is
the re-check loop, (ii) is bounded by the retry budget, (iii) is settled as one `call` per wave
carrying `lineRefs[]`, (iv) is the event-retention ask of §4.4 (D-124): until it is answered an
event delivered between listens can be lost, and only a lost resume is recovered — in the
approval stage's resume wait by the stopgap poll of D-130, and in every other wait that holds a
recorded suspension by the same poll on its tick (D-133); (v) is satisfied by the re-check loop,
which needs no Function, and asks only whether a remainder could be armed directly, which is (i);
and (vi) is assumed: every retry-only `catch` of §3.6 relies on it, and the CI test cannot observe
it, so the plugin's answer is a readiness item (open question Q-11: Q-11 carries the six
sub-questions (i)–(vi)).

### 4.6 Protected operations are never inside a swallowing `catch`

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-workflow-constraint-definition-no-swallowing-catch`

A `try` whose task list contains a `protected` operation **MUST** have a `catch` that either
carries only `retry` or routes to one of the named failure routes below. A `catch` that returns
normally after a protected operation's permanent failure — continuing the path as if the step
had settled — is refused before publish (§2.2 rule 6). ADR-0012 rule 6 states the same rule
(as amended by decision D-126). That a retry-only `catch` re-raises the last error once its limit
is spent is Q-11 (vi); a route that catches a spent timeout names 408 and carries no
`errors.with.type` filter, because the timeout error's type is `…/errors/timeout` (dsl.md
*Timeouts*).

**One failure rule** (decision D-114). A step call's spent retry budget, its spent timeout, and
a permanent refusal the definition does not route **MUST** fault the invocation. The platform
records it `failed`, then `dead_lettered`, and the instance liveness pass raises the
`invocation-dead` task within one pass interval (`01 §3.8`, `01 §4.16`). The task's platform
re-drive resumes at the faulted task under its still-open key: `retryable-failure` leaves the
key `open` (`01 §3.3`). The definition **MUST** catch a failure only where it is the failure of a
subject an operator can act on, and only on these routes:

| Route | Caught | Leads to |
|-------|--------|----------|
| `dispatch-wave1-create`, `dispatch-wave2-activate` | budget or timeout spent (408, 429, 503, 504); 409 is the re-read | a line task per wave line (fragment (c), `07 §3.3`) |
| `compensate-order` | budget or timeout spent, 409 | `awaitCompensationResolution`, then the next pass (`06 §4.7` item 5) |
| `reflect-verdict` | `approval-reflection-refused` (400) | an order-scope task whose retry re-enters `reflectVerdict` (`03 §4.5` item 3) |
| `admit-trigger` on the start path | `prior-instance-active` (409) | the `supersession` retry, then a fault (`02 §4.7` item 7) |

Everything else faults: every listen-arm admission, `apply-hold`, `apply-resume`,
`authorize-cancel`, the evaluations and `begin-fulfillment` of slice 04,
`report-spawn-signal`, `run-cancellation-fence`, `report-outcome`, `create-manual-task` and
`terminate-instance`. A fault on one of them is a failure of Orders or of a dependency the order
cannot proceed without, not of a subject. A task that the failure stage raised for it would have
to re-enter a stage checkpoint through another stage's arms, which is how a resume consumed by
the failure stage once stranded an order in the hold's resume wait. **Precedent**: the
platform's invocation status machine (`failed → dead_lettered` with no `on_failure` handler,
[serverless-runtime `DESIGN.md:458`](../../../../serverless-runtime/docs/DESIGN.md#invocation-status-state-machine))
and the liveness pass that observes it (D-105). **Rationale**: a swallowed failure of `reflect-verdict`, `begin-fulfillment`,
`report-spawn-signal` or `report-outcome` is precisely the divergence between process state and
order state the dual-authority principle exists to prevent; Orders' record would show the step
failed while the definition proceeds as if it had not.

### 4.7 What a definition change may and may not do

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-workflow-constraint-definition-change-scope`

This table is the one statement of which change needs which vehicle (decision D-136); §1.1,
§2.2, ADR-0011's adjustability contract and `../DESIGN.md` §4.7 point here.

| Change | Vehicle | Checked by |
|--------|---------|------------|
| The order of `composable` operations within a stage where no enforced item pins it; which manual-task or escalation arm an answer routes to; `switch` predicates over returned enums, other than the pinned members of rule 1 | A new definition version, published by the job of §4.2 | Rules of §2.2, the fence of §4.1, the behavioural gate |
| Adding or dropping a `composable` operation, other than a `p1` composable on the path §4.1 requires it on | A new definition version | Same |
| The tick of a re-check loop (§3.6 *Fixed waits and re-check loops*); retry attempts, backoff and jitter within the operation's `retry_class`; task timeouts | A new definition version | Rules 4 and 7, the behavioural gate; a tick a slice item fixes, also the publish job's static tick comparison (§4.2 step 1, D-158); the `wave1` timeout, also the job's overdue-window check (D-159) |
| Adding or removing a `listen` arm whose type is in the closed set, other than the shared arms rule 8 requires | A new definition version | Rules 3 and 8 |
| The approval escalation window, the overdue window, the manual-task SLA classes, the partial-failure policy | A write of the seller's policy — a promotion of `owf_seller_policy` rows on the policy channel ([`01 §3.7`](./01-foundation.md#table-owf_seller_policy)), audited; it reaches only records pinned after it (decisions D-134, D-140) | The policy load's bounds check before commit (`07 §4.8` item 8) |
| Approval gate parties, their count and order | The approval routing configuration, which `open-gates` reads (`03 §3.6`) | Not Orders' |
| A new operation; any operation's behaviour, input or output schema, `deadline_ms`, `retry_class` or `protection`; the protected list and its order; the path a `p1` composable is required on | An Orders release of the slice that owns it, then a definition version that uses it | The release pipeline; `owf_step_operation` is compiled (ADR-0012) |
| A refusal or manual-task reason; a table or column, seam call, idempotency family, audit kind or PDP catalogue value | An Orders release | The release pipeline |
| A `listen` type, operator signal, Lifecycle trigger or process event | A PRD-level change, then an Orders release | The release pipeline |
| A `call` to a platform Function; a `run`, `emit`, `for` or `schedule` task | Not permitted by any vehicle | Rules 2 and 7 |
| Re-authorisation after plan freeze; parking a partial failure | Not permitted: the fence orders `evaluate-payment-auth-eligibility` **<** `construct-and-freeze-plan` **<** `begin-fulfillment`, and park is for an unobtainable verdict only (`08 §4.7` item 7) | Rules 1 and 6 |

**Slice constraints.** Each slice's *Constraints this slice places on the definition* has two
kinds of item (decision D-136). An **enforced** item restates a rule of §2.2 or a row of §4.1, and
the validator refuses a version that breaks it through that rule. Every other item is
**canonical-definition guidance**: the canonical version of §3.6 carries it, the behavioural gate
of §4.2 asserts it for every candidate, and a version departs from it only by amending the slice
item in the same change. A guidance item that fixes a tick value the gate cannot exercise within
its budget is held by the publish job's static comparison of every literal tick with its guidance
value (§4.2 step 1, decision D-158). The validator does not refuse a version for a guidance item; the run-time
guards of the operations remain the backstop.

| Slice | Enforced items (rule or §4.1 row) | Canonical-definition guidance |
|-------|-----------------------------------|-------------------------------|
| [`02 §4.7`](./02-triggers-and-start.md#47-constraints-this-slice-places-on-the-definition) | 1 (rule 1, Admission); 4 (rule 1, Unwind); 6 (rule 6); 8, the amendment arm (rule 8); 9 (rule 7) | 2; 3; 5; 7; 8, the route of a `begin-fulfillment` refusal; 10 |
| [`03 §4.5`](./03-approval-execution.md#45-constraints-this-slice-places-on-the-definition) | 1 and 2 (rule 1, Verdict); 3 (rule 6); 4 and 5, the fork, its arms and its calls (rules 1 and 8); 6, the park loop's calls (rule 1, Verdict); 8 (rules 3 and 8) | 4, 5 and 6, the tick values; 4, the fire on every return into the gate loop and the `gate` timeout (D-148); 7 |
| [`04 §4.8`](./04-fulfillment-plan.md#48-constraints-this-slice-places-on-the-definition) | 1 (rule 1, Plan and Waves); 7, the conjunction (rule 1, Waves); 8 (rule 6); 9 (rule 5) | 2; 3; 4; 5; 6; 7, the `PT1H` tick and the undispatched route |
| [`05 §4.5`](./05-provisioning-intents.md#45-constraints-this-slice-places-on-the-definition) | 1 (rule 1, Waves); 2, the `catch` (rule 6); 7, the exported members (rule 5); 9, the `completed` report (§4.1); 12 (rule 4) | 2, the status set; 3; 4; 5; 6; 8; 10; 11 |
| [`06 §4.7`](./06-saga-and-compensation.md#47-constraints-this-slice-places-on-the-definition) | 1 (rule 1, Unwind); 4 (rule 2); 5 (rule 6); 6 (§4.1); 7 (rule 8) | 2; 3; 8 |
| [`07 §4.8`](./07-manual-tasks.md#48-constraints-this-slice-places-on-the-definition) | 1 (rule 1, Failure); 2 (rule 6); 3, the fork and its calls (rule 1, Failure); 5 (rule 1, Failure); 9 (rules 3 and 8) | 3, the `PT5M` tick; 4; 6; 7. Item 8 is checked where the seller's policy is written, not by the validator |
| [`08 §4.7`](./08-hold-and-cancel.md#47-constraints-this-slice-places-on-the-definition) | 1, the order (rule 1, Hold); 2 and 3, the arms (rule 8); 4 (rule 5); 5, the order (rule 1, Unwind); 6 (rule 6); 7 (rules 1 and 6); 8, no walk to a dispatch without `begin-fulfillment` (rule 1, Waves); 9 (rules 3 and 7) | 1, the tick; 2, the placement of the poll and expected-time branches; 5, the return route; 8, the failure route itself; 10 |
| [`09 §4.6`](./09-read-and-authz.md#46-constraints-this-slice-places-on-the-definition) | 1 (rule 2); 2 (rule 5); 3 (rules 1 and 8); 5 (rule 6) | 4 |

## 5. Traceability

- **PRD**: [`../PRD.md`](../PRD.md) — §6.1–§6.4 (the paths), §5.2 (no migration), §15 Q-01 (history isolation criteria), §17.1 (the process-flow diagram these fragments reproduce)
- **ADRs**: [`ADR/0011`](../ADR/0011-cpt-cf-bss-orders-workflow-adr-flow-as-platform-definition.md) flow as platform definition; [`ADR/0012`](../ADR/0012-cpt-cf-bss-orders-workflow-adr-definition-versioning-and-protected-steps.md) definition versioning and protected steps; [`ADR/0013`](../ADR/0013-cpt-cf-bss-orders-workflow-adr-references-not-payloads.md) references not payloads; [`ADR/0004`](../ADR/0004-cpt-cf-bss-orders-workflow-adr-two-wave-activation-barrier.md) two-wave barrier (as amended: the conjunction as a definition pattern); [`ADR/0005`](../ADR/0005-cpt-cf-bss-orders-workflow-adr-saga-compensable-no-pivot.md) compensable no pivot (as amended: `try`/`catch` around an Orders-owned walk); [`ADR/0007`](../ADR/0007-cpt-cf-bss-orders-workflow-adr-fail-closed-verdict-park.md) fail-closed park (as amended: park as a definition arm)
- **Platform**: [serverless-runtime DESIGN](../../../../serverless-runtime/docs/DESIGN.md) §1.1, §1.4, §3.1 (*Functions and Workflows*, *Workflow*, *Versioning Model*, *Invocation Status State Machine*), §3.2 (*Function Registry*), §3.3 (*Function Registry API*, *Invocation API*, *Event Trigger Management API*); [ADR-0003](../../../../serverless-runtime/docs/ADR/0003-cpt-cf-serverless-runtime-adr-workflow-dsl.md); [ADR-0004](../../../../serverless-runtime/docs/ADR/0004-cpt-cf-serverless-runtime-adr-temporal-workflow-engine.md); [ADR-0005](../../../../serverless-runtime/docs/ADR/0005-cpt-cf-serverless-runtime-adr-thin-host.md)
- **Design set**: [`./README.md`](./README.md); [`./01-foundation.md`](./01-foundation.md) (the envelope, the operations, the binding); [`02`](./02-triggers-and-start.md)–[`09`](./09-read-and-authz.md) (the operations each path calls)
- **Related requirements**: `cpt-cf-bss-orders-workflow-fr-owf-start-contract`, `cpt-cf-bss-orders-workflow-fr-owf-approval-request`, `cpt-cf-bss-orders-workflow-fr-owf-approval-escalation`, `cpt-cf-bss-orders-workflow-fr-owf-approval-decision`, `cpt-cf-bss-orders-workflow-fr-owf-payment-auth`, `cpt-cf-bss-orders-workflow-fr-owf-fulfillment-plan`, `cpt-cf-bss-orders-workflow-fr-owf-provisioning-intent`, `cpt-cf-bss-orders-workflow-fr-owf-line-progress`, `cpt-cf-bss-orders-workflow-fr-owf-retry`, `cpt-cf-bss-orders-workflow-fr-owf-manual-task`, `cpt-cf-bss-orders-workflow-fr-owf-compensation-execution`, `cpt-cf-bss-orders-workflow-fr-owf-hold-resume`, `cpt-cf-bss-orders-workflow-fr-owf-overdue-escalation`, `cpt-cf-bss-orders-workflow-fr-owf-terminal-order-events`, `cpt-cf-bss-orders-workflow-fr-owf-process-state-nonauth`
